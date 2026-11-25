//! Themes: colors and materials only (layout lives in config, so sharing a
//! theme never moves someone's dock). Built-in themes are compiled in; user
//! themes live in %APPDATA%\NopBar\themes\*.json.

use std::path::Path;

use serde::{Deserialize, Serialize};

pub const GRAPHITE: &str = include_str!("../themes/graphite.json");

/// How surfaces are finished (Settings > Appearance, ThemeRef::finish):
/// Flat (the tints opaque, no backdrop) or Glass over the blur (glass v3's
/// B, docs/design/harmony/glass-v3: a strong saturation, the wallpaper's
/// color across the plate). The owner's two since 2026-11-11: glass v3's
/// Frost (A) and Clear (C) read as too alike beside it and became Glass
/// (config::OLD_GLASSES). Corners, shadows, spacing and text are the same
/// in both (design system 2.1); the glass is its blur, its tints and its
/// light. Glass needs the blur.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Finish {
    #[default]
    Flat,
    Glass,
}

impl Finish {
    /// The config's text ("flat", "glass"; 0.1.63's "frost" and "clear"
    /// are Glass); anything else is Flat.
    pub fn from_name(n: &str) -> Finish {
        match n.trim().to_ascii_lowercase().as_str() {
            "glass" | "frost" | "clear" => Finish::Glass,
            _ => Finish::Flat,
        }
    }

    /// The glass (it blurs).
    pub fn is_glass(self) -> bool {
        self != Finish::Flat
    }

    /// The glass's coats, light and dark (None: Flat).
    fn recipe(self) -> Option<&'static [Coat; 2]> {
        match self {
            Finish::Flat => None,
            Finish::Glass => Some(&GLASS),
        }
    }
}

/// A surface's material level (design system v2.1, 2.1): M0 the top bar,
/// M1 the dock, M2 panels, M3 menus, M4 name labels and tooltips. The
/// higher, the thicker its tint (more text sits on it), the lighter on the
/// dark palette (one step each: surfaces over each other part without a
/// line) and the nearer its shadow.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Material {
    Bar,
    Dock,
    Panel,
    Menu,
    Tip,
}

impl Material {
    #[allow(dead_code)]
    pub const ALL: [Material; 5] = [Material::Bar, Material::Dock, Material::Panel, Material::Menu, Material::Tip];
}

/// The backdrops body text is held to 4.5:1 over (Theme::plate_seen):
/// the ones a glass really shows. The extremes white and black (a lit and a
/// dark window, a pale and a night wallpaper), a mid grey, and the tones a
/// blurred photo leaves (sky, sea, foliage, meadow, sand, a sunset's
/// orange and pink, dusk violet, a night). FULL_COLORS are apart.
pub const BACKDROPS: [Rgba; 12] = [
    Rgba { r: 255, g: 255, b: 255, a: 255 },
    Rgba { r: 0, g: 0, b: 0, a: 255 },
    Rgba { r: 0x80, g: 0x80, b: 0x80, a: 255 },
    Rgba { r: 0x7F, g: 0xB2, b: 0xE5, a: 255 },
    Rgba { r: 0x2E, g: 0x5E, b: 0x8C, a: 255 },
    Rgba { r: 0x4F, g: 0x79, b: 0x42, a: 255 },
    Rgba { r: 0xA3, g: 0xC2, b: 0x7A, a: 255 },
    Rgba { r: 0xD8, g: 0xC3, b: 0xA0, a: 255 },
    Rgba { r: 0xF0, g: 0x88, b: 0x4A, a: 255 },
    Rgba { r: 0xC8, g: 0x57, b: 0x7A, a: 255 },
    Rgba { r: 0x5B, g: 0x3A, b: 0x86, a: 255 },
    Rgba { r: 0x14, g: 0x1C, b: 0x2C, a: 255 },
];

/// The six full colors (pure red, green, blue, yellow, cyan, magenta): a
/// named exception, held to FULL_COLOR_NEED, not 4.5:1. Through a glass's
/// saturation and Luminosity layer a full color does not come out as a
/// grey of its lightness (a full yellow comes out lighter than white does
/// on the dark palette), and holding body text to 4.5:1 over them took the
/// dark Luminosity layer to .75 to .90: the plate all but opaque on every
/// backdrop, the glass gone, for wallpapers nobody blurs behind a bar
/// (glass-v3/results.md, 4).
pub const FULL_COLORS: [Rgba; 6] = [
    Rgba { r: 255, g: 0, b: 0, a: 255 },
    Rgba { r: 0, g: 255, b: 0, a: 255 },
    Rgba { r: 0, g: 0, b: 255, a: 255 },
    Rgba { r: 255, g: 255, b: 0, a: 255 },
    Rgba { r: 0, g: 255, b: 255, a: 255 },
    Rgba { r: 255, g: 0, b: 255, a: 255 },
];

/// Body text (and secondary text) over BACKDROPS.
pub const BODY_NEED: f32 = 4.5;
/// The menus' faint headers (tertiary text) over any backdrop.
pub const HEADER_NEED: f32 = 2.4;
/// Disabled text (a row that cannot be used) on the panels and the menus
/// over a neutral backdrop, on every finish: WCAG's non-text 3:1, so it
/// still reads, dimmer than everything that can be used.
pub const DISABLED_NEED: f32 = 3.0;
/// The neutral backdrops disabled text is measured over: a dark, a mid
/// and a light grey (the audit's pictures' two and the mid grey).
pub const DISABLED_GREYS: [u8; 3] = [0x3A, 0x80, 0xC4];
/// Body and secondary text over FULL_COLORS: the exception's floor (WCAG's
/// large-text and non-text 3:1).
pub const FULL_COLOR_NEED: f32 = 3.0;

/// Every backdrop text is measured over, with the contrast it keeps there.
pub fn backdrops() -> impl Iterator<Item = (Rgba, f32)> {
    BACKDROPS.into_iter().map(|c| (c, BODY_NEED)).chain(FULL_COLORS.into_iter().map(|c| (c, FULL_COLOR_NEED)))
}

/// A soft shadow of ink's own shape under it: the top bar's clear style's
/// (the mockup's text shadow), laid by the compositor under each cell
/// (topbar::look::HALO_UNDER_WHITE / HALO_UNDER_BLACK, gfx::halo_shadow).
/// Its color, how far down it sits and its blur (CSS's), DIP.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Halo {
    pub color: Rgba,
    pub dy: f32,
    pub blur: f32,
}

/// The glasses' tints, M0 to M4: neutral greys, one step lighter each on
/// the dark palette (surfaces over each other part without a line). What
/// sits on a surface (cards, tiles) is a low-opacity solid fill: never
/// glass on glass.
const GLASS_GREY_DARK: [u8; 5] = [0x1E, 0x22, 0x26, 0x2A, 0x30];
const GLASS_GREY_LIGHT: [u8; 5] = [0xF5, 0xF6, 0xF8, 0xFA, 0xFC];
/// Secondary text over a glass: black or white at 62 %, raised by
/// Theme::glass_dim only as far as every look of the panels and menus needs
/// (their top light and grain, their cards and rows: Theme::plate_seen;
/// 4.5:1 over BACKDROPS, 3:1 over FULL_COLORS).
const GLASS_DIM_DARK: Rgba = Rgba { r: 255, g: 255, b: 255, a: 0x9E };
const GLASS_DIM_LIGHT: Rgba = Rgba { r: 0, g: 0, b: 0, a: 0x9E };

/// One glass in one palette (glass v3's token table, design system 2.1).
/// The plate, bottom up: the DWM blur, saturated by `sat`; the Luminosity
/// layer (the material's grey sets the backdrop's lightness by `lum`, its
/// hue and saturation kept: what keeps text readable without washing the
/// color out); the tint (the same grey at `alpha`, alpha-blended); the
/// top light (`spec`) and the grain (`noise`); the rim (`rim`, white at
/// RIM_STOPS). Everything we paint is achromatic: the color is the
/// wallpaper's alone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coat {
    /// The tint's opacity, M0 to M4 (bar, dock, panel, menu, name label).
    pub alpha: [f32; 5],
    /// The backdrop's saturation (1: as it is; D2D's Saturation effect).
    pub sat: f32,
    /// The Luminosity layer's opacity.
    pub lum: f32,
    /// The rim's white at RIM_STOPS (top, 38 %, bottom: the sides'). All
    /// clear on the built-in glass since glass A (2026-11-12).
    pub rim: [f32; 3],
    /// The top light: white at this at the top, gone at the bottom
    /// (glass::SPEC_LINE; glass A has no glow: SPEC_GLOW).
    pub spec: f32,
    /// The grain's opacity (0: none).
    pub noise: f32,
}

/// Glass (glass v3's B) x [dark, light]: the mockup's recipe
/// (docs/design/harmony/glass-v3/mockup.html, its R table) as plates. The
/// bar and the dock take its plate, panels a step thicker, menus its
/// "strong" plate (text is densest there), name labels stay all but
/// opaque (too small for glass to show). The Luminosity layer stands in
/// for the mockup's brightness (it sets the plate's lightness): it keeps
/// the color where a thicker tint would not (the hue and the saturation
/// stay, only the lightness moves), and it is the lowest (on a .01 step)
/// at which body and secondary text keep 4.5:1 on every surface, card and
/// row over BACKDROPS and 3:1 over FULL_COLORS, the top light at its
/// brightest and the grain included (Theme::plate_seen). Dark .77 (what
/// holds it up is a bright backdrop, a white window or a sand-light photo,
/// under the top light with a card and a selected row on it); light .12.
/// Its panels .60, a hair over the mockup's. Numbers: glass-v3/results.md
/// (10).
///
/// Glass A (2026-11-12, owner: the 0.1.69 glass read as drawn lines): no
/// rim (all clear: no 1 px white edge anywhere), and the top light is a
/// faint fall of white over the whole plate (white .06 dark / .18 light at
/// the top, gone at the bottom: glass::SPEC_LINE), no glow.
const GLASS: [Coat; 2] = [
    Coat { alpha: [0.50, 0.52, 0.60, 0.66, 0.94], sat: 2.0, lum: 0.77, rim: [0.0; 3], spec: 0.06, noise: 0.02 },
    Coat { alpha: [0.48, 0.50, 0.60, 0.66, 0.95], sat: 1.9, lum: 0.12, rim: [0.0; 3], spec: 0.18, noise: 0.02 },
];

/// Windows shell surfaces: persistent Mica-like plates, transient Acrylic.
/// Retains the existing cached blur graph; no new desktop capture or effects.
const WINDOWS_GLASS: [Coat; 2] = [
    Coat { alpha: [0.94, 0.94, 0.78, 0.82, 0.96], sat: 1.0, lum: 0.77, rim: [0.10; 3], spec: 0.015, noise: 0.01 },
    Coat { alpha: [0.94, 0.94, 0.80, 0.84, 0.97], sat: 1.0, lum: 0.12, rim: [0.40; 3], spec: 0.025, noise: 0.01 },
];

/// What a glass plate of one material asks of the compositor (Theme::
/// glass_fx; crate::glass builds it).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlassFx {
    pub sat: f32,
    /// The Luminosity layer: the material's grey, `lum` as alpha.
    pub lum: Rgba,
    pub spec: f32,
    pub noise: f32,
}

/// The built-in theme's JSON tints, opaque (the light steps a little
/// wider apart: opaque, they have only their lightness to part by): what
/// a theme written on Graphite's tokens starts from. The built-in
/// theme's own Flat draws FLAT (flat v2) over them (Theme::apply_flat).
const FLAT_DARK: [u8; 5] = [0x1E, 0x22, 0x26, 0x2A, 0x30];
const FLAT_LIGHT: [u8; 5] = [0xF0, 0xF3, 0xF5, 0xF8, 0xFC];

/// One surface of Flat in one palette (flat v2, "deep opaque": docs/
/// design/harmony/flat-v2, the owner's pick, plan 2). The plate, bottom
/// up: a grey falling from `top` to `bottom` down its height (about 2 %:
/// light from above), the grain over it (`grain`: glass's tile, which
/// also dithers the gradient's 8-bit steps away), the rim (`rim`, white
/// at RIM_STOPS, gathered at the top) with the edge light folded into it
/// (`ring`: white all round just inside the edge, the porcelain lip),
/// two shadows (`near`: stuck to the plate, firm; `far`: the air under
/// it; (y, blur, black's alpha), CSS blur) and the contact shadow.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlatCoat {
    pub top: u8,
    pub bottom: u8,
    pub rim: [f32; 3],
    pub near: (f32, f32, f32),
    pub far: (f32, f32, f32),
    pub contact: f32,
    pub grain: f32,
    pub ring: f32,
}

impl FlatCoat {
    /// The plate's mean grey (what text on it is measured against, and
    /// what a caller that draws one color takes).
    pub const fn mean(&self) -> u8 {
        (self.top as u16 + self.bottom as u16).div_ceil(2) as u8
    }
}

/// Flat's cards (on a panel): opaque greys a step over the panel, lighter
/// hovered, under the panel's top pressed; a light along the top edge
/// (`light`, white: inset 0 1px 0, gone pressed) and a drop just under it
/// (`drop`, black: 0 .5px 1px), so a card reads as raised.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlatCard {
    pub rest: u8,
    pub hover: u8,
    pub pressed: u8,
    pub light: f32,
    pub drop: f32,
}

#[allow(clippy::too_many_arguments)]
const fn flat(top: u8, bottom: u8, rim: [f32; 3], near: (f32, f32, f32), far: (f32, f32, f32), contact: f32, grain: f32, ring: f32) -> FlatCoat {
    FlatCoat { top, bottom, rim, near, far, contact, grain, ring }
}

/// Flat v2, [dark, light] x M0 to M4 (the mockup's T.dark.b / T.light.b).
/// The top bar has no rim, no shadow (it touches the screen's edge). The
/// panels' far shadow is 14 / 34, not the mockup's 36: kept inside
/// metrics::SHADOW_M (14 + 34 = 48, as glass's E3). Its shadows are its
/// own (glass keeps E1 to E4): an opaque plate needs its edge said by a
/// shadow stuck to it, a glass has the blur's (flat-v2/results.md).
///
/// 2026-11-12 (owner: Flat and Glass tell apart at a glance): Flat is
/// paper. One solid grey per plate (each the old fall's mean), no lit rim,
/// no lip, no grain: the light from above is Glass's. Its shadows are
/// short (a plate set down on the desk, not floating).
pub const FLAT: [[FlatCoat; 5]; 2] = [
    [
        flat(0x1D, 0x1D, [0.0; 3], (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), 0.0, 0.0, 0.0),
        flat(0x24, 0x24, [0.0; 3], (1.0, 3.0, 0.36), (6.0, 16.0, 0.34), 0.44, 0.0, 0.0),
        flat(0x29, 0x29, [0.0; 3], (1.0, 3.0, 0.34), (6.0, 16.0, 0.32), 0.44, 0.0, 0.0),
        flat(0x31, 0x31, [0.0; 3], (1.0, 2.0, 0.32), (4.0, 12.0, 0.30), 0.44, 0.0, 0.0),
        flat(0x39, 0x39, [0.0; 3], (1.0, 2.0, 0.30), (2.0, 6.0, 0.22), 0.44, 0.0, 0.0),
    ],
    [
        flat(0xE6, 0xE6, [0.0; 3], (0.0, 0.0, 0.0), (0.0, 0.0, 0.0), 0.0, 0.0, 0.0),
        flat(0xE9, 0xE9, [0.0; 3], (1.0, 3.0, 0.12), (6.0, 16.0, 0.14), 0.11, 0.0, 0.0),
        flat(0xEC, 0xEC, [0.0; 3], (1.0, 3.0, 0.11), (6.0, 16.0, 0.13), 0.11, 0.0, 0.0),
        flat(0xF4, 0xF4, [0.0; 3], (1.0, 2.0, 0.10), (4.0, 12.0, 0.12), 0.11, 0.0, 0.0),
        flat(0xFA, 0xFA, [0.0; 3], (1.0, 2.0, 0.11), (2.0, 6.0, 0.09), 0.11, 0.0, 0.0),
    ],
];

/// Flat v2's cards, [dark, light].
pub const FLAT_CARD: [FlatCard; 2] = [
    FlatCard { rest: 0x34, hover: 0x3B, pressed: 0x2D, light: 0.0, drop: 0.0 },
    // (Light rest F9, not the mockup's FD: hovered FF was two levels over
    // it and read as no hover at all; states-issues, 2026-11-11.)
    FlatCard { rest: 0xF9, hover: 0xFF, pressed: 0xF0, light: 0.0, drop: 0.0 },
];

/// What a plate wears as it is drawn (popup::chrome_at, Launchpad's folder
/// panel, the island's pill and its morph's two ends): its fill from top
/// to bottom (one color: both the same; Flat v2 falls), its rim's stops
/// and how wide (DIP, rim_width; None: none) and its grain (Flat v2's, the
/// Flat one chose; 0: none).
#[derive(Clone, Debug, PartialEq)]
pub struct PlateCoat {
    pub fill: (Rgba, Rgba),
    pub rim: Option<([Rgba; 3], f32)>,
    pub grain: f32,
}

/// Flat v2's card light on one raised thing in one state (Theme::card_fx):
/// the top edge's white (alpha 0: none) and the drop's black.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CardFx {
    pub light: Rgba,
    pub drop: Rgba,
}

fn grey_hex(v: u8) -> String {
    format!("#{v:02X}{v:02X}{v:02X}")
}

/// Pure: Flat v2's grain is drawn: the finish chosen is Flat, and nothing
/// forces a Flat (the blur effect in force: not energy saver, the low
/// presets, software rendering or the switch; Windows' transparency on;
/// no high contrast). A Flat the PC forces is lighter: no effect graph
/// (flat-v2/results.md 2.2).
pub fn flat_grain_on(chosen: Finish, blur: bool, transparency: bool, high_contrast: bool) -> bool {
    chosen == Finish::Flat && blur && transparency && !high_contrast
}

/// The one look in force (12.1): Glass, Flat or high contrast. Windows'
/// transparency off, energy saver and the low presets (no blur) are Flat:
/// rim and shadows stay, only the blur goes. High contrast alone has
/// neither.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Look {
    Glass,
    Flat,
    HighContrast,
}

/// Pure: the look for the finish chosen (`glass`), NopBar's blur in force
/// (`blur`), Windows' transparency and high contrast.
pub fn look_of(glass: bool, blur: bool, transparency: bool, high_contrast: bool) -> Look {
    if high_contrast {
        Look::HighContrast
    } else if glass && blur && transparency {
        Look::Glass
    } else {
        Look::Flat
    }
}

/// The squircle |x|^5 + |y|^5 = 1 read as a circular arc: its radius as a
/// share of the side (spike S6: the largest gap to the squircle is 0.35 px
/// on a 52 px icon; the best fits are .221 to .234).
pub const SQUIRCLE_ARC: f32 = 0.225;

/// Pure: the dock's corner radius around icons `icon` DIP wide inside
/// `pad` DIP of padding: concentric with the icons' squircle (1.3).
#[allow(dead_code)]
pub fn dock_radius(icon: f32, pad: f32) -> f32 {
    (icon.max(0.0) * SQUIRCLE_ARC + pad.max(0.0)).min(40.0)
}

/// A shadow's blur as the tokens write it (CSS's: the Gaussian's sigma is
/// half of it) as Composition's DropShadow.BlurRadius wants it (spike S5:
/// its sigma is .357 of BlurRadius, so 1.4 times the CSS blur).
pub fn composition_blur(css_blur: f32) -> f32 {
    css_blur.max(0.0) * 1.4
}

/// A real window's corners (1.2, `r.window`: Windows rounds them): what
/// stands for a window (Split View's halves) has them too.
pub const WINDOW_RADIUS: f32 = 8.0;

/// A whole control that cannot be used is drawn at this opacity (6.2);
/// text alone that cannot be used takes text_disabled().
#[allow(dead_code)]
pub const DISABLED_OPACITY: f32 = 0.38;
/// The plate that covers the screen (2.1): its grey, light and dark, and
/// how opaque it is over the blur (Theme::scrim).
pub const SCRIM_LIGHT: u8 = 0xF0;
pub const SCRIM_DARK: u8 = 0x18;
pub const SCRIM_OPACITY: f32 = 0.55;

/// The rim's stops down a surface's height (2.2): top, 38 %, bottom.
pub const RIM_STOPS: [f32; 3] = [0.0, 0.38, 1.0];
/// White's alpha at RIM_STOPS: (dark, light).
const RIM: [(f32, f32); 3] = [(0.20, 0.85), (0.07, 0.50), (0.03, 0.15)];
/// The contact shadow's black: (dark, light).
const CONTACT: (f32, f32) = (0.42, 0.09);
/// How far the contact shadow reaches past a surface (DIP), no blur.
pub const CONTACT_W: f32 = 0.5;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

/// The C glass shared by the bar and desktop cards.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AdaptiveGlass {
    pub light: bool,
    pub tint: Rgba,
}

impl AdaptiveGlass {
    pub const SATURATION: f32 = 1.3;

    pub fn new(luminance: f32, previous: Option<bool>) -> Self {
        let light = match previous {
            None => luminance > 0.30,
            Some(true) => luminance >= 0.27,
            Some(false) => luminance > 0.33,
        };
        let (r, g, b, start, progress) = if light {
            (248, 248, 250, 0.48, (luminance - 0.30) / 0.50)
        } else {
            (18, 18, 20, 0.30, luminance / 0.30)
        };
        let a = ((start + 0.18 * progress.clamp(0., 1.)) * 255.).round() as u8;
        Self { light, tint: Rgba { r, g, b, a } }
    }
}

#[cfg(test)]
mod adaptive_glass_tests {
    use super::*;

    #[test]
    fn glass_changes_polarity_only_across_its_thresholds() {
        let light = |l, old| AdaptiveGlass::new(l, old).light;
        assert!(!light(0.30, None));
        assert!(light(0.301, None));
        assert!(!light(0.33, Some(false)));
        assert!(light(0.331, Some(false)));
        assert!(light(0.27, Some(true)));
        assert!(!light(0.269, Some(true)));
        assert_ne!(light(0.30, Some(true)), light(0.30, Some(false)));
    }

    #[test]
    fn tint_matches_the_c_glass_endpoints_and_midpoints() {
        let tint = |l, old| AdaptiveGlass::new(l, old).tint;
        assert_eq!(tint(0., None), Rgba { r: 18, g: 18, b: 20, a: 77 });
        assert_eq!(tint(0.15, None).a, 99);
        assert_eq!(tint(0.30, None).a, 122);
        assert_eq!(tint(0.27, Some(true)), Rgba { r: 248, g: 248, b: 250, a: 122 });
        assert_eq!(tint(0.55, None).a, 145);
        assert_eq!(tint(1., None).a, 168);
    }
}

/// "#RRGGBB" or "#RRGGBBAA" (case-insensitive). Anything else is None.
pub fn parse_color(s: &str) -> Option<Rgba> {
    let h = s.trim().strip_prefix('#')?;
    if !(h.len() == 6 || h.len() == 8) || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some(Rgba { r: byte(0)?, g: byte(2)?, b: byte(4)?, a: if h.len() == 8 { byte(6)? } else { 255 } })
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Backplate {
    pub style: String,
    pub fill: String,
}

impl Default for Backplate {
    fn default() -> Self {
        Self { style: "squircle".into(), fill: "auto".into() }
    }
}

/// One drop shadow layer (DIP; `color` "#RRGGBBAA"). Light comes from
/// above: `y` pushes the shadow down on every dock position.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct ShadowLayer {
    pub x: f32,
    pub y: f32,
    pub blur: f32,
    pub color: String,
}

impl Default for ShadowLayer {
    fn default() -> Self {
        Self { x: 0.0, y: 0.0, blur: 0.0, color: "#00000040".into() }
    }
}

/// The dock with blur off ("Blur and transparency" off, the Low preset):
/// an almost opaque slab with a thin top-to-bottom gradient instead of a
/// flat tint, and its own (lighter) shadow.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Matte {
    pub tint: String,
    pub opacity: f32,
    /// How much lighter the top is and darker the bottom (0..=0.3).
    pub gradient: f32,
    pub shadows: Vec<ShadowLayer>,
}

impl Default for Matte {
    fn default() -> Self {
        Self { tint: "#202024".into(), opacity: 0.92, gradient: 0.04, shadows: Vec::new() }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Surface {
    pub material: String,
    pub tint: String,
    pub opacity: f32,
    pub radius: f32,
    pub border: String,
    pub text: String,
    pub backplate: Backplate,
    /// Dock only: a 1 DIP line just inside the top edge ("" = none).
    pub highlight: String,
    /// Dock only: drop shadows under the bar (none = flat).
    pub shadows: Vec<ShadowLayer>,
    /// Dock only: the look with blur off (absent = the plain opaque tint).
    pub matte: Option<Matte>,
}

impl Default for Surface {
    fn default() -> Self {
        Self {
            material: "acrylic".into(),
            tint: "#1E1E22".into(),
            opacity: 0.43,
            radius: 20.0,
            border: "#FFFFFF3C".into(),
            text: "auto".into(),
            backplate: Backplate::default(),
            highlight: String::new(),
            shadows: Vec::new(),
            matte: None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct IndicatorTheme {
    pub color: String,
}

impl Default for IndicatorTheme {
    fn default() -> Self {
        Self { color: "#EBEBF0DC".into() }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Font {
    /// A family name, or Graphite's default (crate::fonts::THEME_DEFAULT),
    /// which means "the UI font": the user's choice, else Windows'. After
    /// resolve() it is the family every surface draws with.
    pub family: String,
    pub size: f32,
    /// Set by resolve(): the user's text size (crate::fonts::scale), so a
    /// new size counts as a new theme (caches keyed on it are dropped).
    #[serde(skip)]
    pub scale: f32,
}

impl Default for Font {
    fn default() -> Self {
        Self { family: crate::fonts::THEME_DEFAULT.into(), size: 13.0, scale: 1.0 }
    }
}

/// Spacing and type: the same in light and dark (a panel keeps its size
/// when the system switches), shared by every surface (DIP).
pub mod metrics {
    /// A panel's edge to its cards (the container line, 3.3).
    #[allow(dead_code)]
    pub const PANEL_PAD: f32 = space::S3;
    /// A card's edge to its content: the content line is the container
    /// line + this.
    #[allow(dead_code)]
    pub const CARD_PAD: f32 = space::S4;
    /// The type ramp's sizes under their old names (ramp has the full
    /// styles): body text, secondary lines (Footnote), captions. The top
    /// bar's size is the theme's `font.size`.
    pub const BODY: f32 = super::ramp::BODY.size;
    pub const SMALL: f32 = super::ramp::FOOTNOTE.size;
    pub const CAPTION: f32 = super::ramp::CAPTION.size;
    /// Room around a panel for its shadow (the panels' windows are this much
    /// larger on every side); panel shadows are kept inside it (E3's far
    /// shadow: y 16 + blur 32).
    pub const SHADOW_M: f32 = 48.0;
    /// The dock's running dot.
    pub const DOT: f32 = 4.0;

    /// The spacing scale (DIP): every gap and padding is one of these (2
    /// for hairline gaps only: label to its note, dot to icon).
    #[allow(dead_code)]
    pub mod space {
        pub const S1: f32 = 2.0;
        pub const S2: f32 = 4.0;
        pub const S3: f32 = 8.0;
        pub const S4: f32 = 12.0;
        pub const S5: f32 = 16.0;
        pub const S6: f32 = 24.0;
        pub const S7: f32 = 32.0;
        pub const SCALE: [f32; 7] = [S1, S2, S3, S4, S5, S6, S7];
    }

    /// Part sizes (DIP; design system v2.1, 3.2): one place instead of a
    /// constant per screen. The screens adopt them batch by batch.
    #[allow(dead_code)]
    pub mod size {
        use super::space;
        pub const PANEL_PAD: f32 = super::PANEL_PAD;
        pub const CARD_PAD: f32 = super::CARD_PAD;
        /// A menu's edge to its rows.
        pub const MENU_PAD: f32 = space::S2;
        /// Rows by what they hold (3.3): values only; text, a glyph or a
        /// switch; a 28 DIP control; a settings row; two lines.
        pub const ROW_S: f32 = 24.0;
        pub const ROW_M: f32 = 28.0;
        pub const ROW_L: f32 = 32.0;
        pub const ROW_XL: f32 = 40.0;
        pub const ROW_2XL: f32 = 48.0;
        /// A group's header.
        pub const HEAD_H: f32 = 24.0;
        /// Buttons, dropdowns, segmented bars, fields.
        pub const CONTROL_H: f32 = 28.0;
        /// A button as wide as its panel (radius: the card's, not a capsule).
        pub const BUTTON_WIDE_H: f32 = 32.0;
        pub const BUTTON_PAD_X: f32 = space::S4;
        /// A switch and its knob.
        pub const TOGGLE_W: f32 = 36.0;
        pub const TOGGLE_H: f32 = 20.0;
        pub const TOGGLE_KNOB: f32 = 16.0;
        /// A slider's or progress bar's track, and a slider's knob (held:
        /// times KNOB_HELD).
        pub const TRACK_H: f32 = 4.0;
        pub const KNOB: f32 = 16.0;
        pub const KNOB_HELD: f32 = 1.12;
        /// A surface to the screen's edge, and to what opened it.
        pub const EDGE_GAP: f32 = space::S3;
        /// The focus ring: its width and its gap from the element.
        pub const FOCUS_W: f32 = 2.0;
        pub const FOCUS_GAP: f32 = 2.0;
        /// The fading band at a scroll edge (2.5).
        pub const SCROLL_FADE: f32 = 12.0;
        /// Between the dock's icons; a separator's slot is as wide.
        pub const DOCK_GAP: f32 = space::S3;
        /// The top bar's items and pill to the bar's top and bottom.
        pub const BAR_INSET: f32 = space::S1;
        /// A menu's separator slot (the line in its middle).
        pub const SEP_H: f32 = space::S3;
        /// A checkbox: side, radius, stroke.
        pub const CHECK: f32 = 16.0;
        pub const CHECK_R: f32 = 4.0;
        pub const CHECK_STROKE: f32 = 1.5;
        /// Launchpad's search field (a capsule).
        pub const FIELD_L: f32 = 32.0;
        /// The island card's album art, and its radius.
        pub const ART: f32 = 56.0;
        pub const ART_R: f32 = 8.0;
        /// A piece's or PiP's close button (a capsule), its glyph, its
        /// distance from the edge.
        pub const CLOSE: f32 = 24.0;
        pub const CLOSE_GLYPH: f32 = 12.0;
        pub const CLOSE_INSET: f32 = space::S3;
        /// The least height a quiet state's block needs (11.1).
        pub const QUIET_MIN_H: f32 = 96.0;
        /// The progress ring: diameter and stroke.
        pub const SPINNER: f32 = 16.0;
        pub const SPINNER_STROKE: f32 = 1.5;
        /// Glyph sizes: marks, rows and the top bar, cards, quiet states.
        pub const ICON_XS: f32 = 12.0;
        pub const ICON_S: f32 = 16.0;
        pub const ICON_M: f32 = 20.0;
        pub const ICON_L: f32 = 24.0;
    }
}

/// The type ramp (owner's decision, 2026-11-07: macOS-like, body 13):
/// size / line height (DIP) and weight. Only two weights: 400 and 600 (never
/// Bold; Korean in Malgun Gothic has no 600, see gfx::ui_weight). Numbers
/// are drawn with tabular figures (gfx::Gfx::text_layout_tabular) wherever
/// they change. The menu clock (DISPLAY) is the one size off the ramp.
// Shared tokens: the surfaces adopt them one by one (design batches B2,
// B3), so some are not used anywhere yet.
#[allow(dead_code)]
pub mod ramp {
    /// Segoe UI Variable's named instances, by optical size (spike S2:
    /// opsz 8, 10.5 and 36 pt): Small up to 11 DIP, Text for 12 to 19,
    /// Display from 20 (fonts::face_for picks by size).
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub enum Face {
        Small,
        Text,
        Display,
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct TextStyle {
        pub size: f32,
        pub line: f32,
        /// DWRITE_FONT_WEIGHT's number: REGULAR or SEMIBOLD.
        pub weight: u16,
        /// The optical-size instance it is drawn with.
        pub face: Face,
        /// Letter spacing in em (Latin and digits only; Hangul gets none),
        /// for Segoe UI Variable and other families: the built-in Pretendard
        /// has a table of its own (fonts::tracking).
        pub tracking: f32,
    }

    impl TextStyle {
        /// Size and line height at `scale` (the user's text size times the
        /// display's, as the surfaces scale their DIPs).
        pub fn at(self, scale: f32) -> (f32, f32) {
            (self.size * scale, self.line * scale)
        }
    }

    pub const REGULAR: u16 = 400;
    pub const SEMIBOLD: u16 = 600;

    /// Status, shortcuts, tooltips, slider names.
    pub const CAPTION: TextStyle = TextStyle { size: 11.0, line: 14.0, weight: REGULAR, face: Face::Small, tracking: 0.005 };
    /// Secondary lines, name labels, menu bar text.
    pub const FOOTNOTE: TextStyle = TextStyle { size: 12.0, line: 16.0, weight: REGULAR, face: Face::Text, tracking: 0.0 };
    /// Menu and list rows, settings labels and values.
    pub const BODY: TextStyle = TextStyle { size: 13.0, line: 18.0, weight: REGULAR, face: Face::Text, tracking: 0.0 };
    /// Toggle names, section heads, the top bar's app name.
    pub const BODY_STRONG: TextStyle = TextStyle { size: 13.0, line: 18.0, weight: SEMIBOLD, face: Face::Text, tracking: 0.0 };
    /// Card and panel titles.
    pub const HEADLINE: TextStyle = TextStyle { size: 15.0, line: 20.0, weight: SEMIBOLD, face: Face::Text, tracking: -0.005 };
    /// Big numbers (stats), folder and clean-up titles.
    pub const TITLE: TextStyle = TextStyle { size: 20.0, line: 26.0, weight: SEMIBOLD, face: Face::Display, tracking: -0.005 };
    /// Settings page titles, first-run titles.
    pub const LARGE_TITLE: TextStyle = TextStyle { size: 26.0, line: 32.0, weight: SEMIBOLD, face: Face::Display, tracking: 0.0 };
    /// The exception: the menu's big clock.
    pub const DISPLAY: TextStyle = TextStyle { size: 48.0, line: 56.0, weight: SEMIBOLD, face: Face::Display, tracking: 0.0 };

    /// The ramp, smallest first.
    pub const RAMP: [TextStyle; 7] = [CAPTION, FOOTNOTE, BODY, BODY_STRONG, HEADLINE, TITLE, LARGE_TITLE];
}

/// Corner radii (DIP), from the largest surface to the smallest control
/// (design system v2.1, 1.2). Nested shapes are concentric (concentric():
/// panel 20 - pad 8 = card 12 - pad 4 = control 8); capsules are h / 2
/// (capsule()); the dock's is its surface's `radius` (20 in both modes).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(default)]
pub struct Radii {
    /// Panels: control center, stats, clipboard, shelf, search, the island
    /// card, stacks, previews, the PiP bar, folders.
    pub panel: f32,
    /// Cards: now playing, window previews, Launchpad's cards (= tile).
    pub card: f32,
    /// Tiles and tile-like fills in a panel (toggles, thumbnails, setting
    /// cards, rows' hover inside a panel's padding).
    pub tile: f32,
    /// Small controls: buttons, fields, checkboxes, tags.
    pub control: f32,
    /// Menus: context menus, Launchpad's menu, dropdown lists.
    pub menu: f32,
    /// Name labels and tooltips.
    pub tip: f32,
    /// The big sheets in the middle of the screen (Launchpad's folder and
    /// edit sheets).
    pub sheet: f32,
    /// Checkboxes and small marks; the floor of concentric().
    pub inner: f32,
}

impl Default for Radii {
    /// Harmony v2.1: sheet 28, panel 20, menu 12, card / tile 12, control
    /// 8, tooltip 8, inner 4; the same in both modes.
    fn default() -> Self {
        Self { panel: 20.0, card: 12.0, tile: 12.0, control: 8.0, menu: 12.0, tip: 8.0, sheet: 28.0, inner: INNER_RADIUS }
    }
}

impl Radii {
    pub fn light() -> Self {
        Self::default()
    }

    /// The built-in theme's corners in `finish` (owner, 2026-11-12: the
    /// two finishes tell apart at a glance). Flat is paper: small corners
    /// (panel 10, card 8, control 6: crisp, not nested). Glass is round:
    /// big corners, concentric (panel 22 - pad 8 = card 14) and capsule
    /// controls (14: half the 28 DIP control height; D2D keeps a radius to
    /// half the side). Theme files keep their own.
    pub const fn of(finish: Finish) -> Self {
        match finish {
            Finish::Flat => Self { panel: 10.0, card: 8.0, tile: 8.0, control: 6.0, menu: 8.0, tip: 6.0, sheet: 14.0, inner: INNER_RADIUS },
            Finish::Glass => Self { panel: 22.0, card: 14.0, tile: 14.0, control: 14.0, menu: 14.0, tip: 10.0, sheet: 28.0, inner: INNER_RADIUS },
        }
    }
}

/// Pure: the radius of a shape `pad` inside one with radius `outer`, so
/// their corners share a center (nested corners): outer - pad,
/// never below the inner radius (4: at 0 a corner stands square).
pub fn concentric(outer: f32, pad: f32) -> f32 {
    (outer - pad).max(INNER_RADIUS)
}

/// concentric()'s floor (Radii::inner's default).
pub const INNER_RADIUS: f32 = 4.0;

/// Pure: a capsule's radius (half its height).
#[allow(dead_code)]
pub fn capsule(h: f32) -> f32 {
    (h / 2.0).max(0.0)
}

/// Drop shadow levels (design system v2.1, 2.3), two layers each: a near
/// one (small, firm) and a far one (wide, faint: the air under a floating
/// surface). One light, above and in front (2.6): x is always 0, y always
/// down, wherever the surface sits. No outlines anywhere: depth is these,
/// the rim and the fills.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Elevation {
    /// Name labels, tooltips, slider knobs, the island pill.
    E1,
    /// Context menus, dropdown lists, previews.
    E2,
    /// Panels: control center, stats, search, the island card, stacks
    /// (the palette's `ui.shadows`).
    E3,
    /// The dock (its surface's `shadows`).
    E4,
}

#[allow(dead_code)]
impl Elevation {
    pub const ALL: [Elevation; 4] = [Elevation::E1, Elevation::E2, Elevation::E3, Elevation::E4];

    /// Pure: (near, far) as (y, blur, black's alpha 0..1) DIP. The blur is
    /// CSS's (sigma = blur / 2; composition_blur() for a DropShadow).
    pub const fn layers(self, light: bool) -> [(f32, f32, f32); 2] {
        let ((ny, nb, nl, nd), (fy, fb, fl, fd)) = match self {
            Elevation::E1 => ((1.0, 2.0, 0.10, 0.28), (2.0, 6.0, 0.08, 0.22)),
            Elevation::E2 => ((3.0, 8.0, 0.09, 0.28), (10.0, 24.0, 0.12, 0.30)),
            Elevation::E3 => ((6.0, 14.0, 0.10, 0.30), (16.0, 32.0, 0.14, 0.34)),
            Elevation::E4 => ((8.0, 18.0, 0.12, 0.34), (20.0, 40.0, 0.16, 0.36)),
        };
        if light {
            [(ny, nb, nl), (fy, fb, fl)]
        } else {
            [(ny, nb, nd), (fy, fb, fd)]
        }
    }

    /// Pure: how far its shadow reaches past what casts it (DIP): the
    /// further of its layers, offset and blur, Glass's and Flat v2's
    /// (FLAT) alike. The room a surface that draws its own shadow needs
    /// around it, whatever the finish.
    pub fn reach(self) -> f32 {
        let flat = FLAT.iter().flat_map(|p| [p[self.material() as usize].near, p[self.material() as usize].far]);
        self.layers(false).into_iter().chain(flat).map(|(y, blur, _)| y + blur).fold(0.0, f32::max)
    }

    /// The material drawn at this level (2.1).
    pub const fn material(self) -> Material {
        match self {
            Elevation::E1 => Material::Tip,
            Elevation::E2 => Material::Menu,
            Elevation::E3 => Material::Panel,
            Elevation::E4 => Material::Dock,
        }
    }
}

/// Text opacity levels (Windows' TextFillColor*, Apple's label colors):
/// the palette's ink (white on dark, black on light) at these alphas, so
/// text keeps its contrast over a see-through surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextLevels {
    pub primary: f32,
    pub secondary: f32,
    /// Placeholders, group headers' notes (large or secondary text only).
    pub tertiary: f32,
    pub disabled: f32,
}

pub const TEXT_DARK: TextLevels = TextLevels { primary: 0.90, secondary: 0.62, tertiary: 0.42, disabled: 0.30 };
pub const TEXT_LIGHT: TextLevels = TextLevels { primary: 0.88, secondary: 0.60, tertiary: 0.45, disabled: 0.30 };

/// What something that can be pressed is doing (the interaction model,
/// 6.2): one set for every screen.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum State {
    Rest,
    /// The pointer is over it.
    Hover,
    Pressed,
    /// Picked, or on (a list's current row, a switch that is on).
    Selected,
    /// What it opened is still up (a bar item with its menu, a submenu's
    /// parent row): it keeps its pressed look.
    Open,
    /// Cannot be used.
    Disabled,
    /// The keyboard's place in a list (arrow keys): the selected fill, no
    /// ring.
    Cursor,
}

/// What kind of thing it is (6.1).
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    /// Part of a surface: menu and list rows, bar items, glyph buttons.
    /// No fill at rest; ink is laid on it.
    Row,
    /// Raised: buttons, dropdown buttons, cards and tiles that can be
    /// pressed. A card fill at rest; lighter hovered, darker pressed.
    Raised,
    /// Sunk: a switch's or slider's track, a field, a segmented bar's
    /// ground. It does not answer the pointer, only a touch.
    Well,
    /// What moves in a well: a slider's knob, a switch's, a thumb.
    Knob,
}

/// A text level (4.2).
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Level {
    Primary,
    Secondary,
    Tertiary,
}

/// Fills (2.4), (dark, light). Rows and wells are the palette's ink at
/// this alpha; cards are white at theirs in both palettes (a card is
/// lifted towards the light: lighter than its panel, lighter still
/// hovered, under its rest pressed).
pub mod fills {
    pub const ROW_HOVER: (f32, f32) = (0.07, 0.05);
    pub const ROW_PRESSED: (f32, f32) = (0.11, 0.08);
    pub const ROW_SELECTED: (f32, f32) = (0.14, 0.10);
    /// A glass's rows (hover, pressed, selected) x (dark, light): white
    /// pills, the light caught in the glass (owner, 2026-11-12: Glass's
    /// pick is a bright glass pill), not the ink's shade.
    pub const GLASS_ROW: [(f32, f32); 3] = [(0.07, 0.50), (0.11, 0.36), (0.14, 0.85)];
    /// Glass A's pick (Settings' segmented choice and sidebar page), white
    /// (dark, light): a see-through pill, no line, no lit edge, no shadow.
    /// (A glass's selected row, GLASS_ROW's last, is as light; dark it
    /// stays .14: at .20 its secondary text fell under 4.5:1.)
    pub const GLASS_PICK: (f32, f32) = (0.20, 0.85);
    pub const CARD_REST: (f32, f32) = (0.06, 0.70);
    pub const CARD_HOVER: (f32, f32) = (0.10, 0.92);
    pub const CARD_PRESSED: (f32, f32) = (0.04, 0.52);
    /// A glass's light cards (rest, hover, pressed): white still, but as
    /// see-through as the glass under it. Flat's .70 / .92 / .52 made near
    /// opaque white tiles float on a light glass plate (final audit 4:
    /// the calendar's event cards, the control center, the PC's state).
    /// The dark palette's cards are see-through already (.06 / .10 / .04).
    pub const GLASS_CARD_LIGHT: (f32, f32, f32) = (0.36, 0.52, 0.22);
    pub const TRACK: (f32, f32) = (0.14, 0.12);
    /// The one separator inside a card's list, and the dock's.
    pub const HAIRLINE: f32 = 0.08;
    pub const DOCK_SEPARATOR: f32 = 0.16;
}

/// What every surface draws with besides its own material: radii, text and
/// tile colors, the "on" state, the panels' highlight and shadows. One set
/// per palette (top level = dark, `light.ui` = light); a theme without them
/// gets Graphite's.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Ui {
    pub radius: Radii,
    /// Main, secondary and disabled text (what "auto" text means).
    pub text: String,
    pub dim: String,
    pub faint: String,
    /// A card or tile at rest (toggles, buttons, cards in panels): a
    /// see-through fill (fills::CARD_REST), and its 1 DIP edge.
    pub tile: String,
    pub border: String,
    /// Menus' and name labels' tints (materials M3 and M4; the panels' is
    /// `popup.tint`, the dock's and the bar's their surfaces').
    pub menu: String,
    pub tip: String,
    /// A tile that is on, and what is drawn on it.
    pub active: String,
    pub on_active: String,
    /// Slider and progress fill ("accent": the theme's accent, which for
    /// the achromatic accent is the "on" color itself; a colored accent
    /// fills sliders whatever this says).
    pub fill: String,
    /// A 1 DIP line just inside a panel's top edge ("" = none).
    pub highlight: String,
    /// Panels' and menus' drop shadows (kept inside metrics::SHADOW_M).
    pub shadows: Vec<ShadowLayer>,
}

impl Default for Ui {
    /// Graphite dark (look "B", matte graphite).
    fn default() -> Self {
        Self {
            radius: Radii::default(),
            // White at the text levels (TEXT_DARK): .90, .62, .42.
            text: "#FFFFFFE6".into(),
            dim: "#FFFFFF9E".into(),
            faint: "#FFFFFF6B".into(),
            // fills::CARD_REST: white at 6 %.
            tile: "#FFFFFF0F".into(),
            // No outlines: surfaces float on their shadows (high contrast
            // still draws edges in the text color, see resolve()).
            border: "#00000000".into(),
            menu: grey_hex(FLAT_DARK[3]),
            tip: grey_hex(FLAT_DARK[4]),
            // On / selected: a light surface with dark ink (design kit A /
            // AI). No hue anywhere: "on" is lightness contrast.
            active: "#E6E6E6".into(),
            on_active: "#171717".into(),
            fill: "#DADADA".into(),
            highlight: String::new(),
            // Panels sit at E3, inside SHADOW_M.
            shadows: elevation_layers(Elevation::E3, false),
        }
    }
}

/// Elevation `e`'s layers as a theme writes them.
fn elevation_layers(e: Elevation, light: bool) -> Vec<ShadowLayer> {
    e.layers(light)
        .iter()
        .map(|&(y, blur, a)| ShadowLayer { x: 0.0, y, blur, color: format!("#000000{:02X}", (a * 255.0).round() as u8) })
        .collect()
}

impl Ui {
    /// Graphite light (look "A", tidy glass).
    pub fn light() -> Self {
        Self {
            radius: Radii::light(),
            // Black at the text levels (TEXT_LIGHT): .88, .60, .45; no
            // blue-grey (neutral greys only).
            text: "#000000E0".into(),
            dim: "#00000099".into(),
            faint: "#00000073".into(),
            // fills::CARD_REST: white at 70 % (a card is lighter than its
            // panel).
            tile: "#FFFFFFB3".into(),
            border: "#00000000".into(),
            menu: grey_hex(FLAT_LIGHT[3]),
            tip: grey_hex(FLAT_LIGHT[4]),
            // On / selected: near-black with near-white ink (kit A / AI).
            active: "#272727".into(),
            on_active: "#F7F7F7".into(),
            fill: "#363636".into(),
            // No rim: the kit draws no line on any surface.
            highlight: String::new(),
            shadows: elevation_layers(Elevation::E3, true),
        }
    }

    /// The color fields, named, for validate().
    fn colors(&self) -> [(&'static str, &str); 10] {
        [
            ("ui.menu", &self.menu),
            ("ui.tip", &self.tip),
            ("ui.text", &self.text),
            ("ui.dim", &self.dim),
            ("ui.faint", &self.faint),
            ("ui.tile", &self.tile),
            ("ui.border", &self.border),
            ("ui.active", &self.active),
            ("ui.on_active", &self.on_active),
            ("ui.fill", &self.fill),
        ]
    }
}

/// `patch` laid over `base` field by field (objects merged, anything else
/// replaced; null keeps the base): a theme that sets one token keeps the
/// rest of its own palette's set, not the other palette's.
fn merge_json(base: &mut serde_json::Value, patch: serde_json::Value) {
    match (base, patch) {
        (serde_json::Value::Object(b), serde_json::Value::Object(p)) => {
            for (k, v) in p {
                match b.get_mut(&k) {
                    Some(slot) => merge_json(slot, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (_, serde_json::Value::Null) => {}
        (b, p) => *b = p,
    }
}

/// Tokens as written in a theme, over `base`.
fn ui_over<'de, D: serde::Deserializer<'de>>(base: Ui, d: D) -> Result<Ui, D::Error> {
    use serde::de::Error;
    let patch = serde_json::Value::deserialize(d)?;
    let mut v = serde_json::to_value(base).map_err(D::Error::custom)?;
    merge_json(&mut v, patch);
    serde_json::from_value(v).map_err(D::Error::custom)
}

fn dark_ui<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Ui, D::Error> {
    ui_over(Ui::default(), d)
}

fn light_ui<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Ui, D::Error> {
    ui_over(Ui::light(), d)
}

/// The colors that change between light and dark.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Palette {
    pub dock: Surface,
    pub topbar: Surface,
    pub popup: Surface,
    pub indicator: IndicatorTheme,
    /// Absent: Graphite's light set (a light palette wants light tokens).
    #[serde(default = "Ui::light", deserialize_with = "light_ui")]
    pub ui: Ui,
}

impl Default for Palette {
    fn default() -> Self {
        Self { dock: Surface::default(), topbar: Surface::default(), popup: Surface::default(), indicator: IndicatorTheme::default(), ui: Ui::light() }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Theme {
    /// Shell-only styling keeps the deferred desktop widgets/companion intact.
    #[serde(skip)]
    pub(crate) shell_base: Option<Box<Theme>>,
    pub name: String,
    /// "system", "light" or "dark".
    pub mode: String,
    /// "system" (Windows' accent color in the light palette; no hue in the
    /// dark one: lightness only, the "on" color) or "#RRGGBB" (a theme's or
    /// the user's own color, honored as chosen in both).
    pub accent: String,
    /// The dark palette (top-level, as before) ...
    pub dock: Surface,
    pub topbar: Surface,
    pub popup: Surface,
    pub indicator: IndicatorTheme,
    /// The dark palette's shared tokens (see Ui).
    #[serde(deserialize_with = "dark_ui")]
    pub ui: Ui,
    pub font: Font,
    /// ... and the light one (absent = the dark palette in both modes).
    pub light: Option<Palette>,
    /// Set by resolve(): the palette in use is the light one.
    #[serde(skip)]
    pub is_light: bool,
    /// Set by resolve(): the accent color in force (None: the achromatic
    /// "on" color, see accent()).
    #[serde(skip)]
    pub accent_rgba: Option<Rgba>,
    /// Set by resolve(): high contrast colors are in force.
    #[serde(skip)]
    pub high_contrast: bool,
    /// Windows has turned transparency effects off.
    #[serde(skip)]
    pub transparency_off: bool,
    #[serde(skip)]
    pub hc: Option<crate::syslook::HighContrast>,
    /// The finish chosen (ThemeRef::load) before resolve(); after it, the
    /// one in force (Glass falls back to Flat, see resolve_fx()).
    #[serde(skip)]
    pub finish: Finish,
    /// Set by resolve(): the finish chosen (what `finish` held before;
    /// `finish` is then the one in force). What follows the choice and
    /// not the PC (the top bar's style "auto").
    #[serde(skip)]
    pub chosen: Finish,
    /// The built-in theme (Theme::graphite): its Flat is flat v2's
    /// (FLAT: gradients, grain, its own rim and shadows, opaque cards). A
    /// theme file keeps its own solid tints.
    #[serde(skip)]
    pub builtin: bool,
    /// Set by resolve(): Flat's grain is drawn (the Flat one chose, with
    /// the blur effect in force). A Flat the PC forces (a glass fallen
    /// back: transparency off, energy saver, the low presets, software
    /// rendering, the blur off) goes without: lighter, no effect graph.
    #[serde(skip)]
    pub flat_grain: bool,
}

impl Theme {
    pub fn windows_shell(mut self) -> Self {
        if self.shell_base.is_some() { return self; }
        self.shell_base = Some(Box::new(self.clone()));
        if self.builtin {
            self.ui.radius = Radii { panel: 8.0, card: 8.0, tile: 4.0, control: 4.0, menu: 8.0, tip: 8.0, sheet: 8.0, inner: 4.0 };
            self.dock.radius = 8.0;
        }
        if self.glass() { self.apply_glass(); }
        if self.builtin && !self.high_contrast {
            let (near, far) = if self.is_light { ("#00000010", "#00000018") } else { ("#00000024", "#00000030") };
            self.ui.shadows = vec![
                ShadowLayer { x: 0.0, y: 2.0, blur: 6.0, color: near.into() },
                ShadowLayer { x: 0.0, y: 8.0, blur: 16.0, color: far.into() },
            ];
            self.dock.shadows = self.ui.shadows.clone();
        }
        self
    }

    /// Original resolved tokens for surfaces outside the Windows shell brief.
    pub fn non_shell(&self) -> &Self { self.shell_base.as_deref().unwrap_or(self) }

    pub fn graphite() -> Self {
        let mut t: Theme = serde_json::from_str(GRAPHITE).expect("built-in graphite theme is valid JSON");
        t.builtin = true;
        t
    }

    /// `source` is "builtin:<name>" or "file:<path relative to app_dir>".
    /// Unknown or broken themes fall back to Graphite (and say so in the log).
    pub fn load(source: &str, app_dir: &Path) -> Self {
        let loaded = if let Some(name) = source.strip_prefix("builtin:") {
            match name {
                "graphite" => Ok(Self::graphite()),
                other => Err(format!("unknown built-in theme {other:?}")),
            }
        } else if let Some(rel) = source.strip_prefix("file:") {
            std::fs::read_to_string(app_dir.join(rel))
                .map_err(|e| e.to_string())
                .and_then(|t| Self::from_file_text(&t))
        } else {
            Err(format!("bad theme ref {source:?}"))
        };
        loaded.unwrap_or_else(|e| {
            crate::log!("theme: {e}; using Graphite");
            Self::graphite()
        })
    }

    /// A theme file's text, parsed and checked. A theme written before
    /// harmony has no `ui.menu` / `ui.tip`: its menus and name labels keep
    /// following its own `popup` (an empty token, see level_tint()) rather
    /// than Graphite's new greys under the theme's own text color.
    fn from_file_text(text: &str) -> Result<Self, String> {
        let raw: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let mut t: Theme = serde_json::from_value(raw.clone()).map_err(|e| e.to_string())?;
        t.validate()?;
        let unset = |p: &str| raw.pointer(p).is_none();
        if unset("/ui/menu") {
            t.ui.menu.clear();
        }
        if unset("/ui/tip") {
            t.ui.tip.clear();
        }
        if let Some(p) = t.light.as_mut() {
            if unset("/light/ui/menu") {
                p.ui.menu.clear();
            }
            if unset("/light/ui/tip") {
                p.ui.tip.clear();
            }
        }
        Ok(t)
    }

    /// The theme as it should look right now: light or dark palette, the
    /// accent, and the system's transparency / high contrast settings
    /// (those always win: they are accessibility settings), with the
    /// "Blur and transparency" effect as it is in force now.
    pub fn resolve(&self, sys: &crate::syslook::SysLook) -> Theme {
        self.resolve_fx(sys, glass_blur(crate::effects::items().blur, crate::gfx::software_in_force()))
    }

    /// resolve() with the blur effect given (`blur`: NopBar blurs behind
    /// its surfaces; off with energy saver, Auto's conditions, software
    /// rendering or by choice). Glass needs it, Windows' transparency and
    /// no high contrast; otherwise the finish is Flat. Flat draws no
    /// outlines (shadows and fills tell surfaces apart); high contrast
    /// keeps its edges.
    pub fn resolve_fx(&self, sys: &crate::syslook::SysLook, blur: bool) -> Theme {
        let mut t = self.clone();
        t.transparency_off = !sys.transparency;
        let light = match self.mode.as_str() {
            "light" => true,
            "dark" => false,
            _ => sys.light,
        };
        t.is_light = light && self.light.is_some();
        if t.is_light {
            if let Some(p) = &self.light {
                t.dock = p.dock.clone();
                t.topbar = p.topbar.clone();
                t.popup = p.popup.clone();
                t.indicator = p.indicator.clone();
                t.ui = p.ui.clone();
            }
        }
        // "system": Windows' accent on the light palette (a black "on" read
        // as a hole there), lightness only on the dark one (white "on").
        // A color the theme or the user names is honored in both.
        t.accent_rgba = parse_color(&self.accent).or((self.accent.trim() == "system" && t.is_light).then_some(Rgba { a: 255, ..sys.accent }));
        t.font.family = crate::fonts::family_for(&self.font.family);
        t.font.scale = crate::fonts::scale();
        let glass = self.finish.is_glass() && blur && sys.transparency && sys.high_contrast.is_none();
        t.finish = if glass { self.finish } else { Finish::Flat };
        t.chosen = self.finish;
        // The built-in theme's corners follow the finish in force (a
        // glass fallen back is Flat's, high contrast too).
        if self.builtin {
            t.ui.radius = Radii::of(t.finish);
        }
        // The grain on the Flat one chose (not on one the PC forces:
        // Windows' transparency off, energy saver, the low presets,
        // software rendering, the blur off; high contrast draws none).
        t.flat_grain = flat_grain_on(self.finish, blur, sys.transparency, sys.high_contrast.is_some());
        if glass {
            t.apply_glass();
        } else {
            let none = "#00000000";
            for s in [&mut t.dock, &mut t.topbar, &mut t.popup] {
                s.border = none.into();
            }
            t.ui.border = none.into();
        }
        if !sys.transparency {
            // Windows' transparency off is Flat (12.1): the theme's tints
            // opaque, no backdrop. Rim and shadows stay (they are not
            // transparency effects).
            for s in [&mut t.dock, &mut t.topbar, &mut t.popup] {
                s.material = "solid".into();
                s.opacity = 1.0;
            }
        }
        if let Some(hc) = sys.high_contrast {
            let hex = |c: Rgba| format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b);
            for s in [&mut t.dock, &mut t.topbar, &mut t.popup] {
                s.material = "solid".into();
                s.tint = hex(hc.window);
                s.opacity = 1.0;
                s.border = hex(hc.text);
                s.text = hex(hc.text);
                s.backplate.fill = hex(hc.window);
                // No decoration in high contrast.
                s.highlight = String::new();
                s.shadows.clear();
                s.matte = None;
            }
            t.ui.highlight = String::new();
            t.ui.shadows.clear();
            t.ui.menu = hex(hc.window);
            t.ui.tip = hex(hc.window);
            t.indicator.color = hex(hc.text);
            t.accent_rgba = Some(hc.highlight);
            t.hc = Some(hc);
            t.high_contrast = true;
        }
        if t.flat_coat(Material::Panel).is_some() {
            t.apply_flat();
        }
        t
    }

    /// Flat v2's tokens over the built-in theme's (resolve_fx()): each
    /// material's mean grey as its tint (what draws one color, and what
    /// text is measured on), the cards opaque, the panels' and the dock's
    /// shadows Flat's own, and the secondary and tertiary text raised as
    /// far as the plates' lighter tops, the cards and the rows need
    /// (dim_for: dark menus' selected row takes the secondary to .66).
    /// The gradients, the grain and the rim are read where they are drawn
    /// (flat_fill, flat_grain, rim_of).
    fn apply_flat(&mut self) {
        let mean = |t: &Theme, m: Material| grey_hex(t.flat_coat(m).map_or(0, |c| c.mean()));
        self.dock.tint = mean(self, Material::Dock);
        self.topbar.tint = mean(self, Material::Bar);
        self.popup.tint = mean(self, Material::Panel);
        self.ui.menu = mean(self, Material::Menu);
        self.ui.tip = mean(self, Material::Tip);
        let card = FLAT_CARD[self.is_light as usize];
        self.ui.tile = grey_hex(card.rest);
        let layers = |c: &FlatCoat| {
            [c.near, c.far].iter().map(|&(y, blur, a)| ShadowLayer { x: 0.0, y, blur, color: format!("#000000{:02X}", (a * 255.0).round() as u8) }).collect::<Vec<_>>()
        };
        let coats = &FLAT[self.is_light as usize];
        self.ui.shadows = layers(&coats[Material::Panel as usize]);
        self.dock.shadows = layers(&coats[Material::Dock as usize]);
        let base = self.wash(self.text_levels().secondary);
        self.ui.dim = hex8(self.dim_for(base));
        self.ui.faint = hex8(self.glass_faint());
    }

    /// Flat v2's coat of material `m` (None: a glass in force, high
    /// contrast, a theme file: they draw their own way).
    pub fn flat_coat(&self, m: Material) -> Option<&'static FlatCoat> {
        (self.builtin && !self.high_contrast && !self.finish.is_glass()).then(|| &FLAT[self.is_light as usize][m as usize])
    }

    /// Flat v2's cards (None where flat_coat is None).
    pub fn flat_card(&self) -> Option<FlatCard> {
        self.flat_coat(Material::Panel).map(|_| FLAT_CARD[self.is_light as usize])
    }

    /// A card on a menu (the calendar's events): fill_tile, but on Flat v2
    /// a step over the menu's own top, as the panel's card is over the
    /// panel's (the menus, M3, are a step lighter than the panels: the
    /// panel's card was the menu's very grey, the events' cards unseen on
    /// dark: states-issues 6).
    pub fn menu_card(&self) -> Rgba {
        let (Some(card), Some((menu, _)), Some((panel, _))) = (self.flat_card(), self.flat_fill(Material::Menu), self.flat_fill(Material::Panel)) else {
            return self.fill_tile(State::Rest);
        };
        let step = card.rest.abs_diff(panel.r).max(6);
        let v = if self.is_light { menu.r.saturating_add(step).min(0xFF) } else { menu.r.saturating_add(step) };
        Rgba { r: v, g: v, b: v, a: 255 }
    }

    /// Material `m`'s plate as a fall from top to bottom (opaque greys);
    /// None where it is one color (a glass, high contrast, a theme file).
    pub fn flat_fill(&self, m: Material) -> Option<(Rgba, Rgba)> {
        let g = |v: u8| Rgba { r: v, g: v, b: v, a: 255 };
        self.flat_coat(m).map(|c| (g(c.top), g(c.bottom)))
    }

    /// The top bar's opaque band (its solid style): Flat v2's M0 fall
    /// whatever the finish, for the built-in theme (None: a theme file,
    /// high contrast).
    pub fn bar_band(&self) -> Option<(Rgba, Rgba)> {
        let g = |v: u8| Rgba { r: v, g: v, b: v, a: 255 };
        let c = &FLAT[self.is_light as usize][Material::Bar as usize];
        (self.builtin && !self.high_contrast).then(|| (g(c.top), g(c.bottom)))
    }

    /// Flat's grain on material `m` (0: none: not Flat v2, a Flat the PC
    /// forces, name labels).
    pub fn flat_grain(&self, m: Material) -> f32 {
        match self.flat_coat(m) {
            Some(c) if self.flat_grain => c.grain,
            _ => 0.0,
        }
    }

    /// Flat's card light: (the top edge's white, the drop's black) (None:
    /// not Flat v2; cards are see-through fills without light there).
    pub fn card_light(&self) -> Option<(Rgba, Rgba)> {
        let c = self.flat_card()?;
        let a = |v: f32| (v * 255.0).round() as u8;
        Some((Rgba { r: 255, g: 255, b: 255, a: a(c.light) }, Rgba { r: 0, g: 0, b: 0, a: a(c.drop) }))
    }

    /// Flat v2's card light on something raised (Role::Raised: a card, a
    /// tile, a button) in state `s`, as it is drawn (gfx::parts::card_under
    /// / card_over, wash::CardLight): the top edge's white (none pressed or
    /// open: a card pushed in is not lit) and the drop's black under it.
    /// None where a raised thing has no light: not Flat v2 (a glass, a
    /// theme file), high contrast, and the "on" fill (Selected: it stands
    /// alone, the mockup's `.card.on`).
    pub fn card_fx(&self, s: State) -> Option<CardFx> {
        if self.high_contrast || s == State::Selected {
            return None;
        }
        let (light, drop) = self.card_light()?;
        let pushed = matches!(s, State::Pressed | State::Open);
        Some(CardFx { light: if pushed { Rgba { a: 0, ..light } } else { light }, drop })
    }

    /// The glass's tokens over the palette in use (resolve_fx()): each
    /// material's grey at its glass's opacity over the blur. Nothing else
    /// changes but the rim and the top light (glass_fx, rim()): cards,
    /// shadows and corners are Flat's.
    fn apply_glass(&mut self) {
        let Some(coat) = self.coat() else { return };
        let grey = self.glass_grey();
        let none = "#00000000";
        for (s, m) in [(&mut self.dock, Material::Dock), (&mut self.topbar, Material::Bar), (&mut self.popup, Material::Panel)] {
            s.material = "acrylic".into();
            s.tint = grey_hex(grey[m as usize]);
            s.opacity = coat.alpha[m as usize];
            s.border = none.into();
            s.highlight = String::new();
        }
        let with_alpha = |m: Material| format!("{}{:02X}", grey_hex(grey[m as usize]), (coat.alpha[m as usize] * 255.0).round() as u8);
        self.ui.menu = with_alpha(Material::Menu);
        self.ui.tip = with_alpha(Material::Tip);
        self.ui.highlight = String::new();
        self.ui.border = none.into();
        self.ui.dim = hex8(self.glass_dim());
        self.ui.faint = hex8(self.glass_faint());
    }

    /// The glass's tertiary text (the menus' faint headers, placeholders):
    /// its level raised toward the secondary's only as far as a header on
    /// the menus needs HEADER_NEED over every backdrop, and tertiary text
    /// on the panels and the menus 3:1 over a mid grey, as text sees them
    /// (plate_seen). After glass_dim().
    fn glass_faint(&self) -> Rgba {
        let grey = Rgba { r: 0x80, g: 0x80, b: 0x80, a: 255 };
        let headers = backdrops().flat_map(|(wp, _)| self.plate_seen(Material::Menu, wp)).map(|p| (p, HEADER_NEED));
        let on_grey = [Material::Panel, Material::Menu].into_iter().flat_map(|m| self.plate_seen(m, grey)).map(|p| (p, 3.0));
        let unders: Vec<(Rgba, f32)> = headers.chain(on_grey).collect();
        readable(self.text_tertiary(), self.text_secondary().a, &unders)
    }

    /// The glass's secondary text: GLASS_DIM's level raised toward the
    /// primary's only as far as the panels and the menus need as text on
    /// them sees them (plate_seen_all: the top light and the grain at their
    /// bounds, every fallback level, 4.5:1 over BACKDROPS and 3:1 over the
    /// full colors), on the panels'
    /// cards in every state and on the rows of the panels and the menus (a
    /// row on a card draws its text in the primary ink). After the tints
    /// are set (apply_glass).
    fn glass_dim(&self) -> Rgba {
        self.dim_for(if self.is_light { GLASS_DIM_LIGHT } else { GLASS_DIM_DARK })
    }

    /// glass_dim() from `base` (Flat v2 too: its plates' tops and bottoms,
    /// the grain at its bounds, its cards and the rows).
    fn dim_for(&self, base: Rgba) -> Rgba {
        let (rest, hover, pressed) = self.card_states();
        let rows = [State::Hover, State::Pressed, State::Selected].map(|s| self.fill_row(s));
        let mut unders = Vec::new();
        for (p, need) in self.plate_seen_all(Material::Panel) {
            unders.extend([p, over(rest, p), over(hover, p), over(pressed, p)].map(|u| (u, need)));
            unders.extend(rows.iter().map(|&r| (over(r, p), need)));
        }
        for (p, need) in self.plate_seen_all(Material::Menu) {
            unders.push((p, need));
            unders.extend(rows.iter().map(|&r| (over(r, p), need)));
        }
        readable(base, self.text_primary().a, &unders)
    }

    /// The glass in force's coat for the palette in use (None: Flat, high
    /// contrast).
    pub fn coat(&self) -> Option<&'static Coat> {
        if self.high_contrast {
            return None;
        }
        self.finish.recipe().map(|r| {
            if self.shell_base.is_some() { &WINDOWS_GLASS[self.is_light as usize] }
            else { &r[self.is_light as usize] }
        })
    }

    fn glass_grey(&self) -> [u8; 5] {
        if self.is_light {
            GLASS_GREY_LIGHT
        } else {
            GLASS_GREY_DARK
        }
    }

    /// A glass is in force (after resolve()).
    pub fn glass(&self) -> bool {
        self.finish.is_glass() && !self.high_contrast
    }

    /// Material `m`'s plate over an opaque backdrop `wp` (any color: what
    /// the blur leaves of a wallpaper or a window) as text on it sees it,
    /// at its bounds. On a glass: everything it draws, the graph
    /// (glass::model: saturation and the Luminosity layer) under the tint,
    /// at every glass::Level a brush can fall back to (with that level's
    /// tint, glass::tint_at), each with the top light at its brightest (the
    /// top's middle: the line and the glow over it) and without it, and
    /// each of those with the grain at its full alpha and without it. Text
    /// that keeps its contrast against all of them keeps it anywhere on the
    /// plate, whatever the brush. Flat (and labels): the tint alone.
    pub fn plate_seen(&self, m: Material, wp: Rgba) -> Vec<Rgba> {
        self.plate_seen_deeper(m, wp, 0.0)
    }

    /// plate_seen() with the glass's tint made denser by `deepen` (0..1:
    /// another coat of its own grey, glass::fold): the top bar's plate over
    /// a wallpaper its text needs more of (topbar::look::Ground::deepen).
    pub fn plate_seen_deeper(&self, m: Material, wp: Rgba, deepen: f32) -> Vec<Rgba> {
        use crate::glass::{Level, SPEC_GLOW, SPEC_LINE};
        let wp = Rgba { a: 255, ..wp };
        let to8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        if let Some((top, bottom)) = self.flat_fill(m) {
            // Flat v2: its top and its bottom, each with the grain at its
            // full alpha and without it (its coat's, drawn or not: one text
            // level for the Flat one chose and the one the PC forces).
            let grain = Rgba { r: 128, g: 128, b: 128, a: to8(self.flat_coat(m).map_or(0.0, |c| c.grain)) };
            return [top, bottom].into_iter().flat_map(|p| [p, over(grain, p)]).collect();
        }
        let Some(fx) = self.glass_fx(m) else { return vec![over(self.material(m), wp)] };
        let white = |a: f32| Rgba { r: 255, g: 255, b: 255, a: to8(a) };
        // The grain tile: grey at half lightness, its alpha up to all.
        let grain = Rgba { r: 128, g: 128, b: 128, a: to8(fx.noise) };
        let bg = [wp.r, wp.g, wp.b].map(|v| v as f32 / 255.0);
        let mut out = Vec::with_capacity(12);
        for level in [Level::Full, Level::Sat, Level::Plain] {
            let t = crate::glass::tint_at(self, m, level);
            let t = crate::glass::fold(t, Rgba { a: to8(deepen), ..t });
            let p = crate::glass::model(bg, &level.fx(fx), t);
            let plate = Rgba { r: to8(p[0]), g: to8(p[1]), b: to8(p[2]), a: 255 };
            let lit = over(white(fx.spec * SPEC_GLOW.2), over(white(fx.spec * SPEC_LINE[0].1), plate));
            for p in [plate, lit] {
                out.push(p);
                if grain.a > 0 {
                    out.push(over(grain, p));
                }
            }
        }
        out
    }

    /// plate_seen() over every backdrop (backdrops(): BACKDROPS and the
    /// full colors), each look once with the contrast text keeps on it
    /// (the higher where two backdrops give the same look).
    pub fn plate_seen_all(&self, m: Material) -> Vec<(Rgba, f32)> {
        let mut out: Vec<(Rgba, f32)> = backdrops().flat_map(|(wp, need)| self.plate_seen(m, wp).into_iter().map(move |p| (p, need))).collect();
        out.sort_by(|a, b| (a.0.r, a.0.g, a.0.b).cmp(&(b.0.r, b.0.g, b.0.b)).then(b.1.total_cmp(&a.1)));
        out.dedup_by_key(|u| (u.0.r, u.0.g, u.0.b));
        out
    }

    /// What material `m`'s plate asks of the compositor with a glass in
    /// force (None: Flat, high contrast, name labels: they have no
    /// backdrop): the backdrop's saturation, the Luminosity layer, the top
    /// light and the grain.
    pub fn glass_fx(&self, m: Material) -> Option<GlassFx> {
        let coat = self.coat()?;
        if m == Material::Tip {
            return None;
        }
        let g = self.glass_grey()[m as usize];
        Some(GlassFx { sat: coat.sat, lum: Rgba { r: g, g, b: g, a: (coat.lum * 255.0).round() as u8 }, spec: coat.spec, noise: coat.noise })
    }

    /// The accent: the color in force (chosen, or Windows' on the light
    /// palette), else the achromatic "on" color (active()); Highlight in
    /// high contrast. Focus rings,
    /// selections, progress, toggles and marks all draw with it.
    pub fn accent(&self) -> Rgba {
        self.accent_rgba.unwrap_or_else(|| self.active())
    }

    /// Text and glyphs drawn on an accent fill: on_active() for the
    /// achromatic accent, HighlightText in high contrast, else black or
    /// white, whichever reads better on the chosen color.
    pub fn on_accent(&self) -> Rgba {
        if let Some(h) = self.hc {
            return h.highlight_text;
        }
        match self.accent_rgba {
            None => self.on_active(),
            Some(c) => on_color(c),
        }
    }

    /// A window that asks for attention (its running mark): full ink, no
    /// hue; the bounce and the mark's shape carry the news.
    pub fn attention(&self) -> Rgba {
        if let Some(h) = self.hc {
            return h.highlight;
        }
        self.ink()
    }

    /// A card's or tile's states: (rest, hover, pressed), see-through fills
    /// (fills::CARD_*). Transparent in high contrast (cards are outlined
    /// there).
    pub fn card_states(&self) -> (Rgba, Rgba, Rgba) {
        (self.fill_tile(State::Rest), self.fill_tile(State::Hover), self.fill_tile(State::Pressed))
    }

    /// White at the palette's share of a (dark, light) pair: the card
    /// fills. Transparent in high contrast.
    fn card(&self, (dark, light): (f32, f32)) -> Rgba {
        if self.high_contrast {
            return Rgba { r: 0, g: 0, b: 0, a: 0 };
        }
        Rgba { r: 255, g: 255, b: 255, a: ((if self.is_light { light } else { dark }) * 255.0).round() as u8 }
    }

    /// `c` as it looks on the menus' and panels' surface (its tint, opaque).
    fn on_popup(&self, c: Rgba) -> Rgba {
        over(c, Rgba { a: 255, ..self.popup_tint() })
    }

    /// The ink's wash (white on dark, black on light) at the palette's
    /// share of a (dark, light) pair.
    fn wash_of(&self, (dark, light): (f32, f32)) -> Rgba {
        self.wash(if self.is_light { light } else { dark })
    }

    /// A list or menu row's fill (fills::ROW_*; none at rest), translucent:
    /// it sits on any surface. Open keeps the pressed fill, the keyboard's
    /// cursor is the selected one. Transparent in high contrast (the focus
    /// outline marks the row there).
    pub fn fill_row(&self, s: State) -> Rgba {
        if self.glass() && self.builtin {
            let white = |(d, l): (f32, f32)| Rgba { r: 255, g: 255, b: 255, a: ((if self.is_light { l } else { d }) * 255.0).round() as u8 };
            return match s {
                State::Rest | State::Disabled => Rgba { r: 0, g: 0, b: 0, a: 0 },
                State::Hover => white(fills::GLASS_ROW[0]),
                State::Pressed | State::Open => white(fills::GLASS_ROW[1]),
                State::Selected | State::Cursor => white(fills::GLASS_ROW[2]),
            };
        }
        match s {
            State::Rest | State::Disabled => Rgba { r: 0, g: 0, b: 0, a: 0 },
            State::Hover => self.wash_of(fills::ROW_HOVER),
            State::Pressed | State::Open => self.wash_of(fills::ROW_PRESSED),
            State::Selected | State::Cursor => self.wash_of(fills::ROW_SELECTED),
        }
    }

    /// A tile's or card's fill on a panel (fills::CARD_*), translucent, the
    /// same in both finishes. Selected is the "on" fill (active()).
    /// Transparent in high contrast (outlined there).
    pub fn fill_tile(&self, s: State) -> Rgba {
        if self.high_contrast {
            return Rgba { r: 0, g: 0, b: 0, a: 0 };
        }
        if let Some(c) = self.flat_card() {
            let g = |v: u8| Rgba { r: v, g: v, b: v, a: 255 };
            return match s {
                State::Rest | State::Disabled | State::Cursor => g(c.rest),
                State::Hover => g(c.hover),
                State::Pressed | State::Open => g(c.pressed),
                State::Selected => self.active(),
            };
        }
        if let Some((rest, hover, pressed)) = self.glass_cards() {
            return match s {
                State::Rest | State::Disabled | State::Cursor => rest,
                State::Hover => hover,
                State::Pressed | State::Open => pressed,
                State::Selected => self.active(),
            };
        }
        match s {
            State::Rest | State::Disabled | State::Cursor => self.tile(),
            State::Hover => self.card(fills::CARD_HOVER),
            State::Pressed | State::Open => self.card(fills::CARD_PRESSED),
            State::Selected => self.active(),
        }
    }

    /// A glass's light cards (fills::GLASS_CARD_LIGHT: rest, hover,
    /// pressed), white and see-through like the plate under them. None
    /// off a light glass in force (Flat, the dark palette, high contrast,
    /// a theme file's own tile).
    fn glass_cards(&self) -> Option<(Rgba, Rgba, Rgba)> {
        if !(self.glass() && self.is_light && self.builtin) {
            return None;
        }
        let (r, h, p) = fills::GLASS_CARD_LIGHT;
        let white = |a: f32| Rgba { r: 255, g: 255, b: 255, a: (a * 255.0).round() as u8 };
        Some((white(r), white(h), white(p)))
    }

    /// A slider's or progress bar's track (fills::TRACK), translucent; the
    /// text color in high contrast.
    pub fn fill_track(&self) -> Rgba {
        if let Some(h) = self.hc {
            return h.text;
        }
        self.wash_of(fills::TRACK)
    }

    /// Elevation `e`'s shadows as (x, y, blur, color) DIP: the palette's own
    /// for E3 (`ui.shadows`, kept inside SHADOW_M) and E4 (the dock's), the
    /// ramp for E1 and E2. None in high contrast.
    pub fn elevation(&self, e: Elevation) -> Vec<(f32, f32, f32, Rgba)> {
        if self.high_contrast {
            return Vec::new();
        }
        // Flat v2: its own two shadows per surface (FLAT).
        if let Some(c) = self.flat_coat(e.material()) {
            return [c.near, c.far].iter().map(|&(y, blur, a)| (0.0, y, blur, Rgba { r: 0, g: 0, b: 0, a: (a * 255.0).round() as u8 })).collect();
        }
        match e {
            Elevation::E3 => self.panel_shadows(),
            Elevation::E4 => self.dock_shadows(false),
            _ => e
                .layers(self.is_light)
                .iter()
                .map(|&(y, blur, a)| (0.0, y, blur, Rgba { r: 0, g: 0, b: 0, a: (a * 255.0).round() as u8 }))
                .collect(),
        }
    }

    /// The palette's text opacity levels.
    pub fn text_levels(&self) -> TextLevels {
        if self.is_light {
            TEXT_LIGHT
        } else {
            TEXT_DARK
        }
    }

    /// Primary text, translucent (the ink at its level): for surfaces that
    /// draw it over whatever they show. ink() is the same on the menus'
    /// surface, opaque. WindowText in high contrast.
    #[allow(dead_code)]
    pub fn text_primary(&self) -> Rgba {
        if let Some(h) = self.hc {
            return h.text;
        }
        parse_color(&self.popup.text).or_else(|| parse_color(&self.ui.text)).unwrap_or(self.wash(self.text_levels().primary))
    }

    /// Secondary text (shortcuts, notes, values), translucent.
    pub fn text_secondary(&self) -> Rgba {
        if let Some(h) = self.hc {
            return h.text;
        }
        parse_color(&self.ui.dim).unwrap_or(self.wash(self.text_levels().secondary))
    }

    /// Tertiary text (placeholders, faint headers), translucent.
    pub fn text_tertiary(&self) -> Rgba {
        if let Some(h) = self.hc {
            return h.text;
        }
        parse_color(&self.ui.faint).unwrap_or(self.wash(self.text_levels().tertiary))
    }

    /// Disabled text and glyphs: the primary text's color at the disabled
    /// level, translucent.
    /// Raised from the level toward the secondary's only as far as it
    /// keeps DISABLED_NEED on the panels and the menus as text sees them
    /// (disabled_grounds): a row that cannot be used still reads.
    #[allow(dead_code)]
    pub fn text_disabled(&self) -> Rgba {
        if let Some(h) = self.hc {
            return h.gray_text;
        }
        let p = self.text_primary();
        let base = Rgba { a: (self.text_levels().disabled * 255.0).round() as u8, ..p };
        readable(base, self.text_secondary().a, &self.disabled_grounds())
    }

    /// What disabled text is read against, each with DISABLED_NEED: the
    /// panels' and the menus' plates over the neutral greys
    /// (DISABLED_GREYS) as text sees them (plate_seen: every fallback
    /// level, the top light and the grain at their bounds). Flat's plates are its own whatever the backdrop.
    pub fn disabled_grounds(&self) -> Vec<(Rgba, f32)> {
        let mut out = Vec::new();
        for m in [Material::Panel, Material::Menu] {
            for g in DISABLED_GREYS {
                let wp = Rgba { r: g, g, b: g, a: 255 };
                out.extend(self.plate_seen(m, wp).into_iter().map(|p| (p, DISABLED_NEED)));
            }
        }
        out
    }

    /// A progress bar's track (its fill is fill()): fill_track() on the
    /// menus' surface, opaque.
    pub fn track(&self) -> Rgba {
        if let Some(h) = self.hc {
            return h.text;
        }
        self.on_popup(self.fill_track())
    }

    /// Main text on surface `s` ("auto": the palette's primary text level),
    /// opaque: the level as it looks on the surface's own tint (the
    /// translucent tokens are text_primary() and its siblings).
    pub fn text(&self, s: &Surface) -> Rgba {
        let c = parse_color(&s.text).or_else(|| parse_color(&self.ui.text)).unwrap_or(self.wash(self.text_levels().primary));
        let fallback = if self.is_light { Rgba { r: 0xF5, g: 0xF5, b: 0xF5, a: 255 } } else { Rgba { r: 0x24, g: 0x24, b: 0x24, a: 255 } };
        over(c, Rgba { a: 255, ..parse_color(&s.tint).unwrap_or(fallback) })
    }

    /// Text in panels and menus (opaque, see text()).
    pub fn ink(&self) -> Rgba {
        self.text(&self.popup)
    }

    /// Secondary text (shortcuts, captions) or, `disabled`, faint text (the
    /// tertiary level: placeholders, disabled rows), opaque on the menus'
    /// surface (text_secondary() / text_tertiary() are the translucent
    /// tokens; text_disabled() the dimmer disabled level).
    pub fn text_dim(&self, disabled: bool) -> Rgba {
        if self.high_contrast {
            return self.text(&self.popup);
        }
        self.on_popup(if disabled { self.text_tertiary() } else { self.text_secondary() })
    }

    /// A translucent wash over a surface (`a` 0..1): ink-dark on light,
    /// white on dark. Transparent in high contrast (outlines mark things).
    pub fn wash(&self, a: f32) -> Rgba {
        if self.high_contrast {
            return Rgba { r: 0, g: 0, b: 0, a: 0 };
        }
        let a = (a.clamp(0.0, 1.0) * 255.0).round() as u8;
        if self.is_light {
            Rgba { r: 0, g: 0, b: 0, a }
        } else {
            Rgba { r: 255, g: 255, b: 255, a }
        }
    }

    /// A tile at rest. Transparent in high contrast (outlined instead).
    pub fn tile(&self) -> Rgba {
        if self.high_contrast {
            return Rgba { r: 0, g: 0, b: 0, a: 0 };
        }
        if let Some((rest, _, _)) = self.glass_cards() {
            return rest;
        }
        parse_color(&self.ui.tile).unwrap_or(self.card(fills::CARD_REST))
    }

    /// A tile drawn opaque (over an icon, under a pile of pictures): its
    /// see-through fill as it looks on the panel's tint.
    pub fn tile_solid(&self) -> Rgba {
        over(self.tile(), Rgba { a: 255, ..self.popup_tint() })
    }

    /// A tile's or card's 1 DIP edge (the text color in high contrast).
    pub fn tile_border(&self) -> Rgba {
        if let Some(h) = self.hc {
            return h.text;
        }
        parse_color(&self.ui.border).unwrap_or(self.wash(0.12))
    }

    /// A tile that is on (Highlight in high contrast).
    pub fn active(&self) -> Rgba {
        if let Some(h) = self.hc {
            return h.highlight;
        }
        parse_color(&self.ui.active).unwrap_or(self.ink())
    }

    /// Text and glyphs on an active tile.
    pub fn on_active(&self) -> Rgba {
        if let Some(h) = self.hc {
            return h.highlight_text;
        }
        parse_color(&self.ui.on_active).unwrap_or(Rgba { r: 255, g: 255, b: 255, a: 255 })
    }

    /// Slider and progress fill: the accent when it has a color (and for
    /// "accent"), else the theme's fill; Highlight in high contrast.
    pub fn fill(&self) -> Rgba {
        if let Some(h) = self.hc {
            return h.highlight;
        }
        if self.ui.fill == "accent" || self.accent_rgba.is_some() {
            return self.accent();
        }
        parse_color(&self.ui.fill).unwrap_or(self.ink())
    }

    /// Corner radii, kept sane (0..=32 DIP).
    pub fn radii(&self) -> Radii {
        let r = self.ui.radius;
        let c = |v: f32| if v.is_finite() { v.clamp(0.0, 32.0) } else { 0.0 };
        Radii { panel: c(r.panel), card: c(r.card), tile: c(r.tile), control: c(r.control), menu: c(r.menu), tip: c(r.tip), sheet: c(r.sheet), inner: c(r.inner) }
    }

    /// The panels' 1 DIP top highlight, if any (none in high contrast).
    pub fn panel_highlight(&self) -> Option<Rgba> {
        if self.high_contrast {
            return None;
        }
        parse_color(&self.ui.highlight).filter(|c| c.a > 0)
    }

    /// Panel and menu shadows as (x, y, blur, color), DIP, each kept inside
    /// the panels' shadow margin (none in high contrast).
    pub fn panel_shadows(&self) -> Vec<(f32, f32, f32, Rgba)> {
        if self.high_contrast {
            return Vec::new();
        }
        let m = metrics::SHADOW_M;
        self.ui
            .shadows
            .iter()
            .filter_map(|l| {
                let c = parse_color(&l.color)?;
                let (x, y) = (l.x.clamp(-m / 2.0, m / 2.0), l.y.clamp(-m / 2.0, m / 2.0));
                let blur = l.blur.clamp(0.0, m - x.abs().max(y.abs()));
                (c.a > 0).then_some((x, y, blur, c))
            })
            .collect()
    }

    /// Hover / pressed highlight behind bar items and menu rows. In high
    /// contrast there is no fill (text keeps WindowText on Window); the
    /// focus outline (`focus_outline`) marks the row instead.
    pub fn hover(&self, pressed: bool) -> Rgba {
        if self.high_contrast {
            return Rgba { r: 0, g: 0, b: 0, a: 0 };
        }
        // The row fills (fills::ROW_*): white 7 % / 11 % on dark, black 5 % /
        // 8 % on light.
        self.fill_row(if pressed { State::Pressed } else { State::Hover })
    }

    /// High contrast: outline color for the hovered / focused row or item
    /// (Highlight, 2 DIP). None outside high contrast.
    pub fn focus_outline(&self) -> Option<Rgba> {
        self.hc.map(|h| h.highlight)
    }

    /// Separator lines in the dock: the ink at .16 in both palettes.
    pub fn dock_separator(&self) -> Rgba {
        if let Some(h) = self.hc {
            return h.text;
        }
        self.wash(fills::DOCK_SEPARATOR)
    }

    /// Separator lines in menus and panels: the hairline's color (2.4:
    /// one separator, .08; the old .12 / .16 line is gone). Its callers
    /// move to cards or to hairline() with their screens' batches.
    pub fn separator(&self) -> Rgba {
        self.hairline()
    }

    /// Hover name label: (background, text): material M4 (the tips'
    /// tint; opaque on Flat, at its own opacity with Glass, no backdrop).
    pub fn label_colors(&self) -> (Rgba, Rgba) {
        if let Some(h) = self.hc {
            return (h.window, h.text);
        }
        (self.material(Material::Tip), self.ink())
    }

    /// Popup border.
    pub fn popup_border(&self) -> Rgba {
        parse_color(&self.popup.border).unwrap_or(if self.is_light {
            Rgba { r: 255, g: 255, b: 255, a: 0xD9 }
        } else {
            Rgba { r: 0x49, g: 0x49, b: 0x49, a: 255 }
        })
    }

    /// Colors must parse; opacity must be 0..=1.
    pub fn validate(&self) -> Result<(), String> {
        for (what, c) in [
            ("dock.tint", &self.dock.tint),
            ("dock.border", &self.dock.border),
            ("indicator.color", &self.indicator.color),
        ] {
            if parse_color(c).is_none() {
                return Err(format!("{what} is not #RRGGBB[AA]: {c:?}"));
            }
        }
        if !(0.0..=1.0).contains(&self.dock.opacity) {
            return Err(format!("dock.opacity out of range: {}", self.dock.opacity));
        }
        for u in std::iter::once(&self.ui).chain(self.light.as_ref().map(|p| &p.ui)) {
            for (what, c) in u.colors() {
                if parse_color(c).is_none() && !(what == "ui.fill" && c == "accent") {
                    return Err(format!("{what} is not #RRGGBB[AA]: {c:?}"));
                }
            }
            if !u.highlight.is_empty() && parse_color(&u.highlight).is_none() {
                return Err(format!("ui.highlight is not #RRGGBB[AA]: {:?}", u.highlight));
            }
            if let Some(l) = u.shadows.iter().find(|l| parse_color(&l.color).is_none()) {
                return Err(format!("ui shadow color is not #RRGGBB[AA]: {:?}", l.color));
            }
        }
        let palettes = std::iter::once(&self.dock).chain(self.light.as_ref().map(|p| &p.dock));
        for d in palettes {
            if !d.highlight.is_empty() && parse_color(&d.highlight).is_none() {
                return Err(format!("dock.highlight is not #RRGGBB[AA]: {:?}", d.highlight));
            }
            let matte = d.matte.iter().flat_map(|m| m.shadows.iter());
            for l in d.shadows.iter().chain(matte) {
                if parse_color(&l.color).is_none() {
                    return Err(format!("dock shadow color is not #RRGGBB[AA]: {:?}", l.color));
                }
            }
            if let Some(m) = &d.matte {
                if parse_color(&m.tint).is_none() || !(0.0..=1.0).contains(&m.opacity) {
                    return Err(format!("dock.matte: bad tint {:?} or opacity {}", m.tint, m.opacity));
                }
            }
        }
        Ok(())
    }

    /// Dock tint with the theme's opacity applied as alpha.
    pub fn dock_tint(&self) -> Rgba {
        let mut c = parse_color(&self.dock.tint).unwrap_or(Rgba { r: 30, g: 30, b: 34, a: 255 });
        c.a = (self.dock.opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
        c
    }

    /// A theme's own top line token (`dock.highlight`). The dock draws the
    /// rim (rim()) since harmony v2.1 and no longer this; the film's JSON
    /// still says it.
    #[allow(dead_code)]
    pub fn dock_highlight(&self) -> Option<Rgba> {
        parse_color(&self.dock.highlight)
    }

    /// The matte slab (blur off), if the theme has one and the dock's own
    /// material is not solid already (transparency off, high contrast and
    /// solid themes stay as they are).
    pub fn dock_matte(&self) -> Option<&Matte> {
        if self.high_contrast || self.dock.material == "solid" {
            return None;
        }
        self.dock.matte.as_ref()
    }

    /// The dock's shadow layers as (x, y, blur, color), DIP, kept in sane
    /// ranges: the glass shelf's, or the matte slab's with blur off.
    pub fn dock_shadows(&self, matte: bool) -> Vec<(f32, f32, f32, Rgba)> {
        let layers = match (matte, self.dock_matte()) {
            (true, Some(m)) => &m.shadows,
            _ => &self.dock.shadows,
        };
        layers
            .iter()
            .filter_map(|l| {
                let c = parse_color(&l.color)?;
                (c.a > 0).then_some((l.x.clamp(-32.0, 32.0), l.y.clamp(-32.0, 32.0), l.blur.clamp(0.0, 64.0), c))
            })
            .collect()
    }

    pub fn dock_border(&self) -> Rgba {
        parse_color(&self.dock.border).unwrap_or(Rgba { r: 255, g: 255, b: 255, a: 60 })
    }

    /// Fixed backplate color, if the theme sets one ("auto" = derive from
    /// each icon).
    pub fn backplate_fill(&self) -> Option<Rgba> {
        parse_color(&self.dock.backplate.fill)
    }

    /// Top bar tint with its opacity as alpha.
    pub fn topbar_tint(&self) -> Rgba {
        let mut c = parse_color(&self.topbar.tint).unwrap_or(Rgba { r: 20, g: 20, b: 22, a: 255 });
        c.a = (self.topbar.opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
        c
    }

    /// Top bar text ("auto" follows light/dark).
    pub fn topbar_text(&self) -> Rgba {
        self.text(&self.topbar)
    }

    /// Popup menu tint with its opacity as alpha.
    pub fn popup_tint(&self) -> Rgba {
        let mut c = parse_color(&self.popup.tint).unwrap_or(Rgba { r: 26, g: 26, b: 30, a: 255 });
        c.a = (self.popup.opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
        c
    }

    pub fn indicator_color(&self) -> Rgba {
        parse_color(&self.indicator.color).unwrap_or(Rgba { r: 235, g: 235, b: 240, a: 220 })
    }
}

/// WCAG relative luminance of `c` (its alpha ignored): 0 black, 1 white.
pub fn luminance(c: Rgba) -> f32 {
    let lin = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
}

/// Pure: the blur as Glass sees it: the "Blur and transparency" effect in
/// force (`blur`: off with energy saver, Auto's conditions or by choice),
/// and not drawing on the software device (`software`: chosen, or a GPU
/// recovery fell back to it; Glass's layers cost on the CPU there).
pub fn glass_blur(blur: bool, software: bool) -> bool {
    blur && !software
}

/// `c` as "#RRGGBBAA".
fn hex8(c: Rgba) -> String {
    format!("#{:02X}{:02X}{:02X}{:02X}", c.r, c.g, c.b, c.a)
}

/// Pure: translucent text `c` (an ink at some level) that keeps its
/// contrast over each of `unders` (opaque, each with the contrast it needs
/// there: BODY_NEED, FULL_COLOR_NEED): its level as it is, or raised
/// toward alpha `cap` (the primary text's) only as far as that needs;
/// `cap` when even that does not. An opaque `c` is kept.
pub fn readable(c: Rgba, cap: u8, unders: &[(Rgba, f32)]) -> Rgba {
    if c.a == 255 {
        return c;
    }
    let mut unders = unders.to_vec();
    unders.sort_by(|a, b| (a.0.r, a.0.g, a.0.b, a.0.a).cmp(&(b.0.r, b.0.g, b.0.b, b.0.a)).then(b.1.total_cmp(&a.1)));
    unders.dedup_by_key(|u| (u.0.r, u.0.g, u.0.b, u.0.a));
    let reads = |a: u8| unders.iter().all(|&(u, need)| contrast(over(Rgba { a, ..c }, u), u) >= need);
    if reads(c.a) {
        return c;
    }
    // More of the ink only parts it further from what is under it: the
    // first alpha that reads, found by halving.
    let (mut lo, mut hi) = (c.a, cap.max(c.a));
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if reads(mid) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    Rgba { a: hi, ..c }
}

/// WCAG contrast ratio of two opaque colors (1 to 21; body text wants 4.5).
pub fn contrast(a: Rgba, b: Rgba) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// `top` painted over `bottom` (alpha blended, in sRGB as Direct2D does):
/// what a translucent card looks like on the surface under it.
pub fn over(top: Rgba, bottom: Rgba) -> Rgba {
    let a = top.a as f32 / 255.0;
    let mix = |t: u8, b: u8| (t as f32 * a + b as f32 * (1.0 - a)).round() as u8;
    let out_a = (top.a as f32 + bottom.a as f32 * (1.0 - a)).round() as u8;
    Rgba { r: mix(top.r, bottom.r), g: mix(top.g, bottom.g), b: mix(top.b, bottom.b), a: out_a }
}

/// Text on `c`: white whenever it reads (WCAG 4.5, Windows' habit on its
/// accent colors), else black or white, whichever has more contrast.
pub fn on_color(c: Rgba) -> Rgba {
    let l = luminance(c);
    // White's contrast 1.05 / (l + .05) against black's (l + .05) / .05.
    let white = 1.05 / (l + 0.05);
    if white < 4.5 && (l + 0.05) * (l + 0.05) > 0.05 * 1.05 {
        Rgba { r: 0, g: 0, b: 0, a: 255 }
    } else {
        Rgba { r: 255, g: 255, b: 255, a: 255 }
    }
}

// ---- Design batch B3's tokens (2026-11-07) -----------------------------------
// Two accessors the dock, menus, Launchpad and settings needed that B1 left
// out; kept here, apart, so the token section above stays as B1 wrote it.
impl Theme {
    /// A hairline's color (a separator where space alone will not do, drawn
    /// one device pixel thick: popup::hairline): the ink's wash at .08, far
    /// softer than separator(); the text color in high contrast.
    pub fn hairline(&self) -> Rgba {
        if let Some(h) = self.hc {
            return h.text;
        }
        self.wash(fills::HAIRLINE)
    }

    /// A knob's shadow (a switch's, a slider's): Glass A's one soft layer,
    /// E1's far one (owner, 2026-11-12: one soft shadow, no line). None in
    /// high contrast.
    pub fn knob_shadow(&self) -> Vec<(f32, f32, f32, Rgba)> {
        self.elevation(Elevation::E1).into_iter().last().into_iter().collect()
    }

    /// A slider's knob (it carries elevation E1, no outline): white on the
    /// light palette, the ink on the dark one (Apple's light-grey knob, not
    /// a white dot on a dark menu); the text color in high contrast.
    pub fn knob(&self) -> Rgba {
        if let Some(h) = self.hc {
            return h.text;
        }
        if self.is_light {
            Rgba { r: 255, g: 255, b: 255, a: 255 }
        } else {
            self.ink()
        }
    }
}

// ---- Harmony v2.1's tokens (2026-11-11) ---------------------------------------
// Materials, the rim and the contact shadow, and the one place a state's
// fill, outline and text color come from (the interaction model, 6.2, and
// high contrast's table, 12.3). The screens move to fill_of / outline_of /
// text_of with their own batches; card_states, fill_row and the settings'
// own functions stay until then.
#[allow(dead_code)]
impl Theme {
    /// The look in force (after resolve()).
    pub fn look(&self) -> Look {
        look_of(self.finish.is_glass(), true, true, self.high_contrast)
    }

    /// The plate that covers the screen (Launchpad's backdrop; design
    /// system 2.1, the row under the five materials): its tint over the
    /// blur, SCRIM_OPACITY as alpha. No rim, no shadow: it has no edge.
    pub fn scrim(&self) -> Rgba {
        let v = if self.is_light { SCRIM_LIGHT } else { SCRIM_DARK };
        Rgba { r: v, g: v, b: v, a: (SCRIM_OPACITY * 255.0).round() as u8 }
    }

    /// Material `m`'s tint, its opacity as alpha: opaque on Flat, the
    /// Window color in high contrast.
    pub fn material(&self, m: Material) -> Rgba {
        match m {
            Material::Bar => self.topbar_tint(),
            Material::Dock => self.dock_tint(),
            Material::Panel => self.popup_tint(),
            Material::Menu => self.level_tint(&self.ui.menu, m),
            Material::Tip => self.level_tint(&self.ui.tip, m),
        }
    }

    fn level_tint(&self, hex: &str, m: Material) -> Rgba {
        if let Some(h) = self.hc {
            return Rgba { a: 255, ..h.window };
        }
        if hex.is_empty() {
            // A pre-harmony theme (from_file_text()): its panels' material.
            return self.popup_tint();
        }
        let v = (if self.is_light { FLAT_LIGHT } else { FLAT_DARK })[m as usize];
        let c = parse_color(hex).unwrap_or(Rgba { r: v, g: v, b: v, a: 255 });
        if self.glass() {
            c
        } else {
            Rgba { a: 255, ..c }
        }
    }

    /// The rim (2.2): not a line but light along a surface's inner edge,
    /// one device pixel wide, bright at the top and gone by the bottom:
    /// white at RIM_STOPS. On the dock, panels, menus and tips; never the
    /// top bar (it touches the screen's edge). None in high contrast. The
    /// one switch: returning None here turns it off everywhere.
    pub fn rim(&self) -> Option<[Rgba; 3]> {
        self.rim_of(Material::Panel)
    }

    /// rim() on material `m`: Flat v2's own per surface, its edge light
    /// (FlatCoat::ring, white all round) folded in: the mockup draws the
    /// two at the same place and width (its 1 px border and `inset 0 0 0
    /// 1px`), so they are one stroke just inside the edge, rim_width wide.
    /// Everywhere else the same on every surface.
    pub fn rim_of(&self, m: Material) -> Option<[Rgba; 3]> {
        if self.high_contrast {
            return None;
        }
        let white = |a: f32| Rgba { r: 255, g: 255, b: 255, a: (a.clamp(0.0, 1.0) * 255.0).round() as u8 };
        // The glass's own light.
        if let Some(coat) = self.coat() {
            return Some(coat.rim.map(white));
        }
        if let Some(c) = self.flat_coat(m) {
            // (Flat since 2026-11-12: all clear; the light is Glass's.)
            return Some(c.rim.map(|r| white(1.0 - (1.0 - r) * (1.0 - c.ring))));
        }
        Some(RIM.map(|(dark, light)| white(if self.is_light { light } else { dark })))
    }

    /// A plate of material `m` as a panel draws it (PlateCoat): Flat v2's
    /// fall, else `tint` (opaque or not, as the caller has it); the rim,
    /// rim_width wide on a display scaled `s`; the grain.
    pub fn plate_coat(&self, m: Material, tint: Rgba, s: f32) -> PlateCoat {
        PlateCoat { fill: self.flat_fill(m).unwrap_or((tint, tint)), rim: self.rim_of(m).map(|r| (r, self.rim_width(m, s))), grain: self.flat_grain(m) }
    }

    /// How wide material `m`'s rim is drawn (DIP) on a display scaled `s`:
    /// one device pixel (a glass's hairline, a theme file's), Flat v2's
    /// 1 DIP (its edge light is 1 DIP at every scale: the mockup's `inset
    /// 0 0 0 1px`; 1.5 device pixels at 150 %, 2 at 200 %).
    pub fn rim_width(&self, m: Material, s: f32) -> f32 {
        match self.flat_coat(m) {
            Some(_) => 1.0,
            _ => 1.0 / s.max(0.25),
        }
    }

    /// The contact shadow (2.2): black, CONTACT_W past a surface on every
    /// side, no blur, no direction: where it meets what is under it.
    /// Transparent in high contrast.
    pub fn contact(&self) -> Rgba {
        if self.high_contrast {
            return Rgba { r: 0, g: 0, b: 0, a: 0 };
        }
        let a = match self.flat_coat(Material::Panel) {
            Some(c) => c.contact,
            None if self.is_light => CONTACT.1,
            None => CONTACT.0,
        };
        Rgba { r: 0, g: 0, b: 0, a: (a * 255.0).round() as u8 }
    }

    /// The fill of something of `role` in state `s` (6.2; high contrast:
    /// 12.3: no fill but Highlight for pressed, selected, open and the
    /// keyboard's cursor).
    pub fn fill_of(&self, role: Role, s: State) -> Rgba {
        if let Some(h) = self.hc {
            return match (role, s) {
                (Role::Knob, State::Selected) => h.highlight_text,
                (Role::Knob, _) => h.text,
                (_, State::Pressed | State::Selected | State::Open | State::Cursor) => h.highlight,
                _ => Rgba { r: 0, g: 0, b: 0, a: 0 },
            };
        }
        match role {
            Role::Row => self.fill_row(s),
            Role::Raised => self.fill_tile(s),
            // A well does not answer the pointer; on, it is the "on" fill.
            Role::Well => match s {
                State::Selected => self.accent(),
                _ => self.fill_track(),
            },
            // A knob in a well that is on is drawn in the "on" ink.
            Role::Knob => match s {
                State::Selected => self.on_accent(),
                _ => self.knob(),
            },
        }
    }

    /// The outline of something of `role` in state `s`: (color, DIP),
    /// drawn inside its edge. None outside high contrast (no outlines:
    /// fills, the rim and shadows mark things). In high contrast: 1 DIP
    /// WindowText around raised and sunk things, 2 DIP Highlight under the
    /// pointer, none where the Highlight fill already marks the state.
    pub fn outline_of(&self, role: Role, s: State) -> Option<(Rgba, f32)> {
        let h = self.hc?;
        match (role, s) {
            (_, State::Hover) => Some((h.highlight, 2.0)),
            (_, State::Pressed | State::Selected | State::Open | State::Cursor) => None,
            (Role::Raised | Role::Well, _) => Some((h.text, 1.0)),
            (Role::Row | Role::Knob, _) => None,
        }
    }

    /// Text of `level` on something in state `s`, translucent: the level's
    /// ink, the disabled level when it cannot be used. High contrast:
    /// WindowText at every level (rank is size, weight and place only),
    /// HighlightText on a Highlight fill.
    pub fn text_of(&self, level: Level, s: State) -> Rgba {
        if let Some(h) = self.hc {
            return match s {
                State::Pressed | State::Selected | State::Open | State::Cursor => h.highlight_text,
                // GrayText: a row that cannot be used is told from the rest
                // (states-issues: it was WindowText like every other).
                State::Disabled => h.gray_text,
                _ => h.text,
            };
        }
        if s == State::Disabled {
            return self.text_disabled();
        }
        match level {
            Level::Primary => self.text_primary(),
            Level::Secondary => self.text_secondary(),
            Level::Tertiary => self.text_tertiary(),
        }
    }

    /// High contrast lays the Highlight fill under something in `ground`
    /// (a selected row, the keyboard's cursor): what lies on it (a
    /// failure row's button) answers on that, not on Window.
    fn on_highlight(&self, ground: State) -> Option<crate::syslook::HighContrast> {
        self.hc.filter(|_| matches!(ground, State::Pressed | State::Selected | State::Open | State::Cursor))
    }

    /// fill_of for something of `role` in state `s` lying on something in
    /// state `ground` (a button in a row). Outside high contrast, and on a
    /// row at rest or under the pointer, it is fill_of. High contrast on
    /// the Highlight fill (12.3, the colors' one pair): no fill, and
    /// pressed it is inverted again (HighlightText), so each state still
    /// shows on the Highlight.
    pub fn fill_on(&self, role: Role, s: State, ground: State) -> Rgba {
        match self.on_highlight(ground) {
            Some(h) if role != Role::Knob => match s {
                State::Pressed | State::Selected | State::Open | State::Cursor => h.highlight_text,
                _ => Rgba { r: 0, g: 0, b: 0, a: 0 },
            },
            _ => self.fill_of(role, s),
        }
    }

    /// outline_of on `ground` (see fill_on). High contrast on the
    /// Highlight fill: the outlines in HighlightText (a Highlight line
    /// under the pointer would be lost on it), as wide as on Window; none
    /// pressed (its own fill marks it).
    pub fn outline_on(&self, role: Role, s: State, ground: State) -> Option<(Rgba, f32)> {
        match self.on_highlight(ground) {
            Some(h) if role != Role::Knob => match s {
                State::Pressed | State::Selected | State::Open | State::Cursor => None,
                _ => self.outline_of(role, s).map(|(_, w)| (h.highlight_text, w)),
            },
            _ => self.outline_of(role, s),
        }
    }

    /// text_of on `ground` (see fill_on). High contrast on the Highlight
    /// fill: HighlightText, and Highlight on the inverted press.
    pub fn text_on(&self, level: Level, s: State, ground: State) -> Rgba {
        match self.on_highlight(ground) {
            Some(h) => match s {
                State::Pressed | State::Selected | State::Open | State::Cursor => h.highlight,
                _ => h.highlight_text,
            },
            None => self.text_of(level, s),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn windows_shell_materials_keep_deferred_surfaces_unchanged() {
        for light in [false, true] {
            let mut raw = Theme::graphite(); raw.finish = Finish::Glass;
            let old = raw.resolve_fx(&crate::syslook::SysLook { light, transparency: true, ..Default::default() }, true);
            let shell = old.clone().windows_shell();
            assert_eq!(shell.non_shell(), &old);
            assert_eq!((shell.radii().panel, shell.radii().menu, shell.radii().control, shell.radii().tip, shell.dock.radius), (8.0, 8.0, 4.0, 8.0, 8.0));
            assert_eq!(shell.font, old.font);
            let coat = shell.coat().unwrap();
            assert!(coat.alpha[Material::Dock as usize] > coat.alpha[Material::Panel as usize]);
            assert_eq!(coat.sat, 1.0);
            assert!(shell.rim_of(Material::Menu).is_some());
        }
    }
    use super::*;

    #[test]
    fn contrast_is_wcags() {
        let (black, white) = (Rgba { r: 0, g: 0, b: 0, a: 255 }, Rgba { r: 255, g: 255, b: 255, a: 255 });
        assert!((contrast(black, white) - 21.0).abs() < 0.01);
        assert!((contrast(white, white) - 1.0).abs() < 1e-6);
        // WCAG's own example: #777777 on white is just under 4.5.
        let grey = Rgba { r: 0x77, g: 0x77, b: 0x77, a: 255 };
        assert!((contrast(grey, white) - 4.48).abs() < 0.01);
        // Half black over white: mid grey, opaque.
        assert_eq!(over(Rgba { a: 128, ..black }, white), Rgba { r: 127, g: 127, b: 127, a: 255 });
    }

    #[test]
    fn parse_color_variants() {
        assert_eq!(parse_color("#1E1E22"), Some(Rgba { r: 0x1E, g: 0x1E, b: 0x22, a: 255 }));
        assert_eq!(parse_color("#ffffff3c"), Some(Rgba { r: 255, g: 255, b: 255, a: 0x3C }));
        assert_eq!(parse_color("1E1E22"), None);
        assert_eq!(parse_color("#12345"), None);
        assert_eq!(parse_color("#GG0000"), None);
    }

    #[test]
    fn builtin_graphite_loads_and_matches_current_look() {
        let t = Theme::load("builtin:graphite", Path::new("."));
        assert_eq!(t.name, "Graphite");
        assert!(t.validate().is_ok());
        // Harmony v2.1, Flat (the default finish): the dock is material M1,
        // #222222 opaque, no backdrop, no edge, radius 20 (as light), its
        // elevation E4.
        // (As written; resolved, Flat v2 draws its own: below.)
        assert_eq!(t.dock_tint(), Rgba { r: 0x22, g: 0x22, b: 0x22, a: 255 });
        assert_eq!(t.dock.material, "solid");
        // No outlines (borderless look): the shadows and the rim carry the edge.
        assert_eq!(t.dock_border(), Rgba { r: 0, g: 0, b: 0, a: 0 });
        assert_eq!(t.indicator_color(), Rgba { r: 0xD9, g: 0xD9, b: 0xD9, a: 255 });
        assert!(t.light.is_some());
        assert_eq!(t.dock.radius, 20.0);
        // (The old top line token is not used: the rim is theme::rim.)
        assert!(t.dock_highlight().is_none());
        assert_eq!(t.dock_shadows(false), vec![(0.0, 8.0, 18.0, Rgba { r: 0, g: 0, b: 0, a: 0x57 }), (0.0, 20.0, 40.0, Rgba { r: 0, g: 0, b: 0, a: 0x5C })]);
        // Resolved: Flat's dock (one grey, 24; its short shadows).
        let dark = t.resolve(&crate::syslook::SysLook::default());
        assert_eq!(dark.dock_tint(), Rgba { r: 0x24, g: 0x24, b: 0x24, a: 255 });
        assert_eq!(dark.dock_shadows(false), vec![(0.0, 1.0, 3.0, Rgba { r: 0, g: 0, b: 0, a: 0x5C }), (0.0, 6.0, 16.0, Rgba { r: 0, g: 0, b: 0, a: 0x57 })]);
        // No matte slab: a plate's fill is one color (2.6), and Flat is
        // opaque already.
        assert!(t.dock.matte.is_none() && t.dock_matte().is_none());
        assert_eq!(t.dock_shadows(true), t.dock_shadows(false));
        // The top bar's text: the ramp's Body (design B2: 12 -> 13).
        assert_eq!(t.font.size, ramp::BODY.size);
        // Light: #F3F3F3 opaque, radius 20, two soft black shadows.
        let light = t.resolve(&crate::syslook::SysLook { light: true, ..Default::default() });
        assert_eq!(light.dock_tint(), Rgba { r: 0xE9, g: 0xE9, b: 0xE9, a: 255 });
        assert_eq!(light.dock_border(), Rgba { r: 0, g: 0, b: 0, a: 0 });
        assert_eq!(light.dock.radius, 20.0);
        assert!(light.dock_highlight().is_none());
        assert!(light.dock_shadows(false).iter().all(|s| (s.3.r, s.3.g, s.3.b) == (0, 0, 0)), "black shadows, no tint");
        assert_eq!(light.dock_shadows(false).len(), 2);
        // A theme's `popup.radius` (menus read radii().menu now) is still
        // read, and ignored.
        let old: Theme = serde_json::from_str(r##"{"popup":{"radius":3}}"##).unwrap();
        assert_eq!(old.radii().menu, 12.0);
    }

    #[test]
    fn graphite_tokens_per_mode() {
        use crate::syslook::SysLook;
        let t = file_graphite();
        let dark = t.resolve(&SysLook { light: false, ..Default::default() });
        let light = t.resolve(&SysLook { light: true, ..Default::default() });
        // Harmony's geometry, the same in both modes.
        assert_eq!(dark.radii(), Radii { panel: 20.0, card: 12.0, tile: 12.0, control: 8.0, menu: 12.0, tip: 8.0, sheet: 28.0, inner: 4.0 });
        assert_eq!(light.radii(), dark.radii());
        // Text: the levels (white .90 / .62 on dark, black .88 / .60 on
        // light) as they look on the panels' surface (#262626 / #F5F5F5).
        let on = |t: &Theme, c: Rgba| over(c, Rgba { a: 255, ..t.popup_tint() });
        assert_eq!(dark.ink(), on(&dark, Rgba { r: 255, g: 255, b: 255, a: 0xE6 }));
        assert_eq!(dark.ink(), Rgba { r: 0xEA, g: 0xEA, b: 0xEA, a: 255 });
        assert_eq!(light.ink(), Rgba { r: 0x1E, g: 0x1E, b: 0x1E, a: 255 });
        assert_eq!(dark.text_dim(false), on(&dark, Rgba { r: 255, g: 255, b: 255, a: 0x9E }));
        assert_eq!(light.text_dim(false), Rgba { r: 0x62, g: 0x62, b: 0x62, a: 255 });
        // Cards and tiles: a see-through white fill in both palettes.
        assert_eq!(dark.tile(), Rgba { r: 255, g: 255, b: 255, a: 0x0F });
        assert_eq!(light.tile(), Rgba { r: 255, g: 255, b: 255, a: 0xB3 });
        // Harmony's default on tones are achromatic in both palettes.
        assert_eq!((dark.active(), dark.on_active()), (Rgba { r: 0xE6, g: 0xE6, b: 0xE6, a: 255 }, Rgba { r: 0x17, g: 0x17, b: 0x17, a: 255 }));
        assert_eq!((light.active(), light.on_active()), (Rgba { r: 0x27, g: 0x27, b: 0x27, a: 255 }, Rgba { r: 0xF7, g: 0xF7, b: 0xF7, a: 255 }));
        assert_eq!((dark.accent(), light.accent()), (dark.active(), light.active()));
        assert_eq!((dark.fill(), light.fill()), (Rgba { r: 0xDA, g: 0xDA, b: 0xDA, a: 255 }, parse_color(&light.ui.fill).unwrap()));
        // The bar is material M0, opaque on Flat.
        assert_eq!(light.topbar_tint(), Rgba { r: 0xF0, g: 0xF0, b: 0xF0, a: 255 });
        assert_eq!(dark.topbar_tint(), Rgba { r: 0x1E, g: 0x1E, b: 0x1E, a: 255 });
        // No top line token on any panel; two black shadows each.
        assert!(dark.panel_highlight().is_none() && dark.panel_shadows().len() == 2);
        assert!(light.panel_highlight().is_none() && light.panel_shadows().len() == 2);
        // Every UI token is a grey in both palettes (no blue-grey left).
        for u in [&t.ui, &t.light.as_ref().unwrap().ui] {
            for (what, c) in u.colors() {
                let p = parse_color(c).unwrap();
                assert!(p.r == p.g && p.g == p.b, "{what} {c} has a hue");
            }
        }
        // Labels sit on material M4.
        assert_eq!(dark.label_colors().0, Rgba { r: 0x30, g: 0x30, b: 0x30, a: 255 });
        assert_eq!(light.label_colors().0, Rgba { r: 0xFC, g: 0xFC, b: 0xFC, a: 255 });
        // The JSON spells out the same sets as the code's defaults.
        assert_eq!(t.ui, Ui::default());
        assert_eq!(t.light.as_ref().map(|p| p.ui.clone()), Some(Ui::light()));
    }

    #[test]
    fn materials_step_up_and_flat_is_glass_without_the_blur() {
        use crate::syslook::SysLook;
        let hex = |c: Rgba| (c.r, c.a);
        for (light, flat, grey) in [(false, FLAT_DARK, GLASS_GREY_DARK), (true, FLAT_LIGHT, GLASS_GREY_LIGHT)] {
            for finish in [Finish::Glass] {
                let sys = SysLook { light, ..Default::default() };
                let f = file_graphite().resolve_fx(&sys, true);
                let mut raw = Theme::graphite();
                raw.finish = finish;
                let g = raw.resolve_fx(&sys, true);
                assert_eq!((f.look(), g.look(), g.finish), (Look::Flat, Look::Glass, finish));
                let coat = &GLASS[light as usize];
                assert_eq!(g.coat(), Some(coat));
                for (i, m) in Material::ALL.into_iter().enumerate() {
                    // Flat: the tint opaque. A glass: its grey at its opacity.
                    assert_eq!(hex(f.material(m)), (flat[i], 255), "flat {m:?} light {light}");
                    assert_eq!(hex(g.material(m)), (grey[i], (coat.alpha[i] * 255.0).round() as u8), "{finish:?} {m:?} light {light}");
                    let c = f.material(m);
                    assert!(c.r == c.g && c.g == c.b, "a grey");
                }
                // Each level a step lighter than the one under it (dark: that
                // is how surfaces over each other part without a line).
                let steps: Vec<u8> = Material::ALL.iter().map(|&m| f.material(m).r).collect();
                assert!(steps.windows(2).all(|w| w[1] > w[0]), "{steps:?}");
                let thick: Vec<u8> = Material::ALL.iter().map(|&m| g.material(m).a).collect();
                assert!(thick.windows(2).all(|w| w[1] > w[0]), "the higher, the thicker: {thick:?}");
                // Flat has no backdrop anywhere; a glass has the blur.
                assert!([&f.dock, &f.topbar, &f.popup].iter().all(|s| s.material == "solid" && s.opacity == 1.0));
                assert!([&g.dock, &g.topbar, &g.popup].iter().all(|s| s.material == "acrylic"));
                // The corners are each finish's own (Flat's small, Glass's
                // round); shadows, the contact shadow and the fills are the
                // same. Glass A (2026-11-12) has no rim: all clear.
                // (`f` is a theme file: its own corners whatever the finish.)
                assert_eq!((f.radii(), g.radii()), (Radii::default(), Radii::of(Finish::Glass)));
                assert_eq!(Theme::graphite().resolve_fx(&sys, true).radii(), Radii::of(Finish::Flat));
                assert_eq!(f.contact(), g.contact());
                assert_eq!(g.rim().map(|r| r.map(|c| c.a)), Some([0; 3]), "{finish:?}");
                for e in Elevation::ALL {
                    assert_eq!(f.elevation(e), g.elevation(e), "{e:?}");
                }
                // (A light glass's cards are see-through like its plate:
                // fills::GLASS_CARD_LIGHT, light_glass_cards_are_see_through.)
                for s in [State::Rest, State::Hover, State::Pressed, State::Selected] {
                    // (A light glass's rows are white pills: GLASS_ROW.)
                    if !light || s == State::Rest {
                        assert_eq!(f.fill_row(s), g.fill_row(s), "{s:?}");
                    }
                    if !light || s == State::Selected {
                        assert_eq!(f.fill_tile(s), g.fill_tile(s), "{s:?}");
                    }
                }
            }
        }
        // The dark tints are the spec's ladder: 1E, 22, 26, 2A, 30.
        assert_eq!(FLAT_DARK, GLASS_GREY_DARK);
        // Elevation and material go together.
        assert_eq!(Elevation::ALL.map(|e| e.material()), [Material::Tip, Material::Menu, Material::Panel, Material::Dock]);
    }

    #[test]
    fn the_look_is_one_of_three() {
        use crate::syslook::SysLook;
        // (finish chosen Glass, blur in force, Windows' transparency, high contrast)
        for glass in [false, true] {
            for blur in [false, true] {
                for transparency in [false, true] {
                    for hc in [false, true] {
                        let want = if hc {
                            Look::HighContrast
                        } else if glass && blur && transparency {
                            Look::Glass
                        } else {
                            Look::Flat
                        };
                        assert_eq!(look_of(glass, blur, transparency, hc), want, "{glass} {blur} {transparency} {hc}");
                        // And what a resolved theme says.
                        let raw = if glass { glass_raw() } else { Theme::graphite() };
                        let sys = SysLook { transparency, high_contrast: hc.then(hc_black), ..Default::default() };
                        assert_eq!(raw.resolve_fx(&sys, blur).look(), want);
                    }
                }
            }
        }
        // Transparency off, energy saver, a low preset: Flat, with its rim
        // and its shadows (only high contrast has neither).
        let off = glass_raw().resolve_fx(&SysLook { transparency: false, ..Default::default() }, true);
        assert!(off.look() == Look::Flat && off.rim().is_some() && !off.panel_shadows().is_empty() && off.contact().a > 0);
        let mean = FLAT[0][Material::Panel as usize].mean();
        assert_eq!(off.material(Material::Panel), Rgba { r: mean, g: mean, b: mean, a: 255 }, "Flat v2's panel, opaque");
        let hc = glass_raw().resolve_fx(&SysLook { high_contrast: Some(hc_black()), ..Default::default() }, true);
        assert!(hc.look() == Look::HighContrast && hc.rim().is_none() && hc.panel_shadows().is_empty() && hc.contact().a == 0);
        // High contrast: every material is the Window color.
        assert!(Material::ALL.iter().all(|&m| hc.material(m) == Rgba { r: 0, g: 0, b: 0, a: 255 }));
    }

    #[test]
    fn the_rim_is_light_from_above_and_the_contact_a_thin_dark() {
        use crate::syslook::SysLook;
        let byte = |v: f32| (v * 255.0).round() as u8;
        for (light, top, mid, bottom, contact) in [(false, 0.20, 0.07, 0.03, 0.42), (true, 0.85, 0.50, 0.15, 0.09)] {
            let t = file_graphite().resolve(&SysLook { light, ..Default::default() });
            let rim = t.rim().expect("a rim");
            assert_eq!(rim.map(|c| (c.r, c.g, c.b)), [(255, 255, 255); 3], "white");
            assert_eq!(rim.map(|c| c.a), [byte(top), byte(mid), byte(bottom)]);
            // Brightest at the top, almost gone at the bottom: light, not
            // an even line all round.
            assert!(rim[0].a > rim[1].a && rim[1].a > rim[2].a && rim[2].a * 4 < rim[0].a);
            assert_eq!(t.contact(), Rgba { r: 0, g: 0, b: 0, a: byte(contact) });
        }
        assert_eq!(RIM_STOPS, [0.0, 0.38, 1.0]);
        assert_eq!(CONTACT_W, 0.5);
    }

    #[test]
    fn states_come_from_one_place() {
        use crate::syslook::SysLook;
        let none = Rgba { r: 0, g: 0, b: 0, a: 0 };
        let all = [State::Rest, State::Hover, State::Pressed, State::Selected, State::Open, State::Disabled, State::Cursor];
        for light in [false, true] {
            let t = file_graphite().resolve(&SysLook { light, ..Default::default() });
            // Part of a surface: no fill at rest, ink laid on it; what it
            // opened keeps it pressed; the keyboard's cursor is selected.
            assert_eq!(t.fill_of(Role::Row, State::Rest), none);
            assert_eq!(t.fill_of(Role::Row, State::Hover), t.fill_row(State::Hover));
            assert_eq!(t.fill_of(Role::Row, State::Open), t.fill_row(State::Pressed));
            assert_eq!(t.fill_of(Role::Row, State::Cursor), t.fill_row(State::Selected));
            assert_eq!(t.fill_of(Role::Row, State::Disabled), none);
            let a = [State::Hover, State::Pressed, State::Selected].map(|s| t.fill_of(Role::Row, s).a);
            assert!(a[0] < a[1] && a[1] < a[2], "hover < pressed < selected: {a:?}");
            // Raised: a card fill at rest, towards the light hovered,
            // under its rest pressed, the "on" fill selected.
            let (rest, hover, pressed) = (t.fill_of(Role::Raised, State::Rest), t.fill_of(Role::Raised, State::Hover), t.fill_of(Role::Raised, State::Pressed));
            assert!(rest.a > 0 && hover.a > rest.a && pressed.a < rest.a, "{rest:?} {hover:?} {pressed:?}");
            assert_eq!((rest.r, hover.r, pressed.r), (255, 255, 255), "white in both palettes");
            assert_eq!(t.fill_of(Role::Raised, State::Open), pressed);
            assert_eq!(t.fill_of(Role::Raised, State::Selected), t.active());
            assert_eq!(t.card_states(), (rest, hover, pressed));
            // Sunk: it does not answer the pointer.
            let well = t.fill_of(Role::Well, State::Rest);
            assert_eq!(well, t.fill_track());
            assert_eq!((t.fill_of(Role::Well, State::Hover), t.fill_of(Role::Well, State::Pressed)), (well, well));
            assert_eq!(t.fill_of(Role::Well, State::Selected), t.accent());
            assert_eq!((t.fill_of(Role::Knob, State::Rest), t.fill_of(Role::Knob, State::Selected)), (t.knob(), t.on_accent()));
            // No outline anywhere outside high contrast.
            for role in [Role::Row, Role::Raised, Role::Well, Role::Knob] {
                for s in all {
                    assert_eq!(t.outline_of(role, s), None, "{role:?} {s:?}");
                }
            }
            // Text: the level's ink; the disabled level when it cannot be used.
            assert_eq!(t.text_of(Level::Primary, State::Rest), t.text_primary());
            assert_eq!(t.text_of(Level::Secondary, State::Hover), t.text_secondary());
            assert_eq!(t.text_of(Level::Tertiary, State::Rest), t.text_tertiary());
            assert_eq!(t.text_of(Level::Primary, State::Disabled), t.text_disabled());
        }
        assert_eq!(DISABLED_OPACITY, 0.38);
        // High contrast (12.3): system colors only.
        let hc = Theme::graphite().resolve(&SysLook { high_contrast: Some(hc_black()), ..Default::default() });
        let (white, black, yellow) = (Rgba { r: 255, g: 255, b: 255, a: 255 }, Rgba { r: 0, g: 0, b: 0, a: 255 }, Rgba { r: 255, g: 255, b: 0, a: 255 });
        // Raised and sunk things are outlined, 1 DIP in the text color.
        assert_eq!(hc.outline_of(Role::Raised, State::Rest), Some((white, 1.0)));
        assert_eq!(hc.outline_of(Role::Well, State::Rest), Some((white, 1.0)));
        // Under the pointer: no fill, a 2 DIP Highlight outline.
        for role in [Role::Row, Role::Raised, Role::Well] {
            assert_eq!(hc.fill_of(role, State::Hover), none);
            assert_eq!(hc.outline_of(role, State::Hover), Some((yellow, 2.0)));
            // Pressed, selected, open, the cursor: the Highlight fill and
            // HighlightText on it, no outline besides.
            for s in [State::Pressed, State::Selected, State::Open, State::Cursor] {
                assert_eq!((hc.fill_of(role, s), hc.outline_of(role, s), hc.text_of(Level::Secondary, s)), (yellow, None, black), "{role:?} {s:?}");
            }
        }
        assert_eq!((hc.fill_of(Role::Knob, State::Rest), hc.fill_of(Role::Knob, State::Selected)), (white, black));
        // Every text level is WindowText: rank is size, weight and place.
        for l in [Level::Primary, Level::Secondary, Level::Tertiary] {
            assert_eq!(hc.text_of(l, State::Rest), white);
        }
    }

    /// The panels' review (2026-11-11, P2): a failure row's button kept
    /// its Window colors on a selected row's Highlight. Each row state x
    /// button state, in each palette (one whose WindowText is its
    /// Highlight among them), is measured as drawn: the button's text on
    /// what is really under it (its fill, else the row's), and its outline
    /// against the row.
    #[test]
    fn a_button_on_a_selected_row_answers_on_the_highlight() {
        use crate::syslook::{HighContrast, SysLook};
        let c = |r: u8, g: u8, b: u8| Rgba { r, g, b, a: 255 };
        let palettes = [
            hc_black(),
            HighContrast { window: c(255, 255, 255), text: c(0, 0, 0), highlight: c(0x37, 0x00, 0x6E), highlight_text: c(255, 255, 255), gray_text: crate::syslook::GRAY_TEXT },
            // WindowText and Highlight the same (a custom theme).
            HighContrast { window: c(0, 0, 0), text: c(0x1A, 0xEB, 0xFF), highlight: c(0x1A, 0xEB, 0xFF), highlight_text: c(0, 0, 0), gray_text: crate::syslook::GRAY_TEXT },
        ];
        for hc in palettes {
            let t = Theme::graphite().resolve(&SysLook { high_contrast: Some(hc), ..Default::default() });
            for ground in [State::Rest, State::Hover, State::Cursor, State::Selected] {
                let under = match t.fill_of(Role::Row, ground) {
                    f if f.a > 0 => f,
                    _ => hc.window,
                };
                let mut looks = Vec::new();
                for s in [State::Rest, State::Hover, State::Pressed] {
                    let fill = t.fill_on(Role::Raised, s, ground);
                    let back = if fill.a > 0 { fill } else { under };
                    let ink = t.text_on(Level::Primary, s, ground);
                    let pair = contrast(hc.highlight_text, hc.highlight).min(contrast(hc.text, hc.window));
                    assert!(contrast(ink, back) >= pair - 0.01, "{hc:?} on {ground:?}, {s:?}: {ink:?} on {back:?}");
                    if let Some((line, _)) = t.outline_on(Role::Raised, s, ground) {
                        assert_ne!(line, under, "{hc:?} on {ground:?}, {s:?}: its line is lost on the row");
                    }
                    looks.push((fill, ink, t.outline_on(Role::Raised, s, ground)));
                }
                // Rest, the pointer and a press still look apart.
                assert!(looks[0] != looks[1] && looks[1] != looks[2] && looks[0] != looks[2], "{hc:?} on {ground:?}: {looks:?}");
            }
            // On a row at rest or under the pointer: the usual colors.
            for s in [State::Rest, State::Hover, State::Pressed] {
                for ground in [State::Rest, State::Hover] {
                    assert_eq!(t.fill_on(Role::Raised, s, ground), t.fill_of(Role::Raised, s));
                    assert_eq!(t.outline_on(Role::Raised, s, ground), t.outline_of(Role::Raised, s));
                    assert_eq!(t.text_on(Level::Primary, s, ground), t.text_of(Level::Primary, s));
                }
            }
        }
        // Outside high contrast nothing changes with the row.
        let t = Theme::graphite().resolve(&SysLook::default());
        for s in [State::Rest, State::Hover, State::Pressed] {
            assert_eq!(t.fill_on(Role::Raised, s, State::Cursor), t.fill_of(Role::Raised, s));
            assert_eq!(t.outline_on(Role::Raised, s, State::Cursor), None);
            assert_eq!(t.text_on(Level::Primary, s, State::Cursor), t.text_of(Level::Primary, s));
        }
    }

    #[test]
    fn the_squircle_reads_as_an_arc_of_0_225() {
        // Spike S6: the gap between |x|^5 + |y|^5 = 1 and a rounded square
        // of radius r (shares of the side), over the corner.
        let gap = |r: f32| {
            let c = 0.5 - r;
            (0..=2000)
                .map(|i| {
                    let (s, k) = (i as f32 / 2000.0 * std::f32::consts::FRAC_PI_2).sin_cos();
                    let (x, y) = (0.5 * k.abs().powf(0.4), 0.5 * s.abs().powf(0.4));
                    if x > c && y > c { ((x - c).hypot(y - c) - r).abs() } else { (x - 0.5).max(y - 0.5).abs() }
                })
                .fold(0.0f32, f32::max)
        };
        // .225: under 0.7 % of the side (0.35 px on a 52 px icon), and
        // better than the steps either side of it.
        assert!(gap(SQUIRCLE_ARC) < 0.007, "{}", gap(SQUIRCLE_ARC));
        assert!(gap(SQUIRCLE_ARC) < gap(0.20) && gap(SQUIRCLE_ARC) < gap(0.25));
        assert!(gap(SQUIRCLE_ARC) * 52.0 < 0.36);
        // The dock around its icons (1.3): 52 gives today's 20.
        assert!((dock_radius(52.0, 8.0) - 19.7).abs() < 1e-4);
        assert!((dock_radius(36.0, 8.0) - 16.1).abs() < 1e-4);
        assert!((dock_radius(96.0, 8.0) - 29.6).abs() < 1e-4);
        assert_eq!(dock_radius(128.0, 8.0), 36.8);
        assert_eq!(dock_radius(400.0, 8.0), 40.0, "kept sane");
        // A DropShadow's radius for a CSS blur (spike S5).
        assert!((composition_blur(32.0) - 44.8).abs() < 1e-4);
        assert_eq!(composition_blur(-1.0), 0.0);
    }

    #[test]
    fn default_accent_is_achromatic_and_explicit_colors_are_honored() {
        use crate::syslook::{HighContrast, SysLook};
        let blue = Rgba { r: 0x00, g: 0x78, b: 0xD4, a: 255 };
        let t = file_graphite();
        assert_eq!(t.accent, "");
        // The default: lightness only on dark (white "on", dark ink) ...
        let dark = t.resolve(&SysLook { light: false, accent: blue, ..Default::default() });
        assert_eq!((dark.accent(), dark.on_accent()), (dark.active(), dark.on_active()));
        assert_eq!(dark.accent_rgba, None);
        assert_eq!(dark.fill(), Rgba { r: 0xDA, g: 0xDA, b: 0xDA, a: 255 }, "the dark fill as before");
        // The default light palette also uses the achromatic Harmony on color.
        let light = t.resolve(&SysLook { light: true, accent: blue, ..Default::default() });
        assert_eq!(light.accent(), light.active());
        assert_eq!(light.on_accent(), light.on_active());
        assert_eq!(light.fill(), parse_color(&light.ui.fill).unwrap());
        assert_eq!(light.accent_rgba, None);
        // An explicit system accent remains available; the default stays neutral.
        for choice in ["", "system"] {
            let r = crate::config::ThemeRef { accent: choice.into(), ..Default::default() }.load(Path::new("."));
            assert_eq!(r.resolve(&SysLook { light: true, accent: blue, ..Default::default() }).accent(), if choice == "system" { blue } else { light.active() }, "{choice:?}");
            assert_eq!(r.resolve(&SysLook { light: false, accent: blue, ..Default::default() }).accent_rgba, None, "{choice:?}");
        }
        // A theme's or the user's own color is honored in both modes, with
        // readable ink.
        let mut mine = t.clone();
        mine.accent = "#FFD60A".into();
        for light in [false, true] {
            let m = mine.resolve(&SysLook { light, accent: blue, ..Default::default() });
            assert_eq!(m.accent(), Rgba { r: 0xFF, g: 0xD6, b: 0x0A, a: 255 });
            assert_eq!(m.on_accent(), Rgba { r: 0, g: 0, b: 0, a: 255 });
            assert_eq!(m.fill(), m.accent());
        }
        mine.accent = "#0A84FF".into();
        assert_eq!(mine.resolve(&SysLook::default()).on_accent(), Rgba { r: 0, g: 0, b: 0, a: 255 });
        assert_eq!(on_color(Rgba { r: 0x27, g: 0x27, b: 0x27, a: 255 }), Rgba { r: 255, g: 255, b: 255, a: 255 });
        // White wins whenever it reads (4.5): Windows' blue takes white
        // ink although black is a hair stronger there.
        assert!(contrast(Rgba { r: 255, g: 255, b: 255, a: 255 }, blue) >= 4.5);
        assert!(contrast(Rgba { r: 0, g: 0, b: 0, a: 255 }, blue) > contrast(Rgba { r: 255, g: 255, b: 255, a: 255 }, blue));
        assert_eq!(on_color(blue), Rgba { r: 255, g: 255, b: 255, a: 255 });
        assert_eq!(light.on_accent(), light.on_active());
        // A theme without a light palette stays achromatic on "system".
        let part: Theme = serde_json::from_str(r##"{"ui":{"fill":"accent"}}"##).unwrap();
        let p = part.resolve(&SysLook { light: true, accent: blue, ..Default::default() });
        assert_eq!(p.fill(), p.active());
        // High contrast keeps its system colors.
        let white = Rgba { r: 255, g: 255, b: 255, a: 255 };
        let black = Rgba { r: 0, g: 0, b: 0, a: 255 };
        let yellow = Rgba { r: 255, g: 255, b: 0, a: 255 };
        let hc = t.resolve(&SysLook { high_contrast: Some(HighContrast { window: black, text: white, highlight: yellow, highlight_text: black, gray_text: crate::syslook::GRAY_TEXT }), ..Default::default() });
        assert_eq!((hc.accent(), hc.on_accent(), hc.attention()), (yellow, black, yellow));
        assert_eq!(hc.card_states().1.a, 0);
        assert_eq!(hc.track(), white);
        // The card fills, per mode, see-through white: dark 10 % hovered,
        // light 52 % pressed.
        assert_eq!(dark.card_states().1, Rgba { r: 255, g: 255, b: 255, a: 26 });
        assert_eq!(light.card_states().2, Rgba { r: 255, g: 255, b: 255, a: 133 });
        assert_eq!(dark.attention(), dark.ink());
    }

    #[test]
    fn partial_tokens_keep_their_palettes_set() {
        let light_ui = |json: &str| serde_json::from_str::<Theme>(json).unwrap().light.unwrap().ui;
        // Omitted, empty: the light set.
        assert_eq!(light_ui(r##"{"light":{}}"##), Ui::light());
        assert_eq!(light_ui(r##"{"light":{"ui":{}}}"##), Ui::light());
        // One color: that color, the rest light (not dark).
        let one = light_ui(r##"{"light":{"ui":{"tile":"#EEEEEE"}}}"##);
        assert_eq!(one, Ui { tile: "#EEEEEE".into(), ..Ui::light() });
        // One radius: that corner, the other corners light.
        let r = light_ui(r##"{"light":{"ui":{"radius":{"panel":16}}}}"##);
        assert_eq!(r, Ui { radius: Radii { panel: 16.0, ..Radii::light() }, ..Ui::light() });
        // Dark: partial tokens over the dark set; null keeps the default.
        let dark: Theme = serde_json::from_str(r##"{"ui":{"radius":{"card":3},"dim":null}}"##).unwrap();
        assert_eq!(dark.ui, Ui { radius: Radii { card: 3.0, ..Radii::default() }, ..Ui::default() });
        // A wrong type is still a broken theme (falls back on load).
        assert!(serde_json::from_str::<Theme>(r##"{"ui":{"radius":"round"}}"##).is_err());
    }


    #[test]
    fn brand_mark_follows_custom_popup_instead_of_mode() {
        for (mode, tint, text, expected) in [("dark", "#FFFFFF", "#000000", true), ("light", "#000000", "#FFFFFF", false)] {
            let json = format!(r##"{{"name":"Custom","mode":"{mode}","popup":{{"material":"solid","tint":"{tint}","opacity":1.0,"text":"{text}"}},"ui":{{"dim":"{text}"}}}}"##);
            let t = Theme::from_file_text(&json).unwrap().resolve_fx(&crate::syslook::SysLook { light: mode == "light", ..Default::default() }, false);
            assert_eq!(crate::gfx::panel_mark_light(&t), expected, "{mode}");
        }
    }

    #[test]
    fn a_pre_harmony_theme_keeps_its_menus_on_its_popup() {
        use crate::syslook::SysLook;
        // White menus with black text, written before ui.menu existed.
        let old = Theme::from_file_text(
            r##"{"name":"Old","popup":{"material":"solid","tint":"#FFFFFF","opacity":1.0,"text":"#000000"}}"##,
        )
        .unwrap();
        let t = old.resolve_fx(&SysLook { light: false, ..Default::default() }, false);
        let menu = t.material(Material::Menu);
        assert_eq!(menu, t.popup_tint(), "the menu follows the theme's own popup");
        assert_eq!((menu.r, menu.g, menu.b, menu.a), (255, 255, 255, 255));
        assert_eq!(t.material(Material::Tip), t.popup_tint());
        // A theme that names the new tokens gets them.
        let new = Theme::from_file_text(r##"{"name":"New","ui":{"menu":"#123456","tip":"#654321"}}"##).unwrap();
        let t = new.resolve_fx(&SysLook { light: false, ..Default::default() }, false);
        let m = t.material(Material::Menu);
        assert_eq!((m.r, m.g, m.b, m.a), (0x12, 0x34, 0x56, 255));
        // The built-in theme is untouched.
        let g = Theme::graphite().resolve_fx(&SysLook { light: false, ..Default::default() }, false);
        assert_ne!(g.material(Material::Menu), g.popup_tint());
    }

    #[test]
    fn tokens_default_and_stay_sane() {
        use crate::syslook::{HighContrast, SysLook};
        // A theme from before the tokens: Graphite's, light ones in light.
        let old: Theme = serde_json::from_str(r##"{"name":"Old","dock":{"tint":"#102030"},"light":{"dock":{"tint":"#F0F0F0"}}}"##).unwrap();
        assert!(old.validate().is_ok());
        assert_eq!(old.ui, Ui::default());
        assert_eq!(old.light.as_ref().map(|p| p.ui.clone()), Some(Ui::light()));
        assert_eq!(old.resolve(&SysLook { light: true, ..Default::default() }).radii(), Radii::light());
        // Partial tokens: the rest from the defaults.
        let part: Theme = serde_json::from_str(r##"{"ui":{"radius":{"panel":4},"fill":"accent"}}"##).unwrap();
        assert!(part.validate().is_ok());
        assert_eq!(part.radii(), Radii { panel: 4.0, ..Radii::default() });
        assert_eq!(part.fill(), part.accent());
        // Wild values are clamped; shadows stay inside the panel margin.
        let wild: Theme = serde_json::from_str(r##"{"ui":{"radius":{"panel":400,"tile":-3},"shadows":[{"y":40,"blur":90,"color":"#000000FF"}]}}"##).unwrap();
        assert_eq!((wild.radii().panel, wild.radii().tile), (32.0, 0.0));
        let (_, y, blur, _) = wild.panel_shadows()[0];
        assert!(y.abs() + blur <= metrics::SHADOW_M);
        // Bad token colors make a bad theme.
        let bad: Theme = serde_json::from_str(r##"{"ui":{"tile":"grey"}}"##).unwrap();
        assert!(bad.validate().is_err());
        let bad: Theme = serde_json::from_str(r##"{"light":{"ui":{"shadows":[{"color":"x"}]}}}"##).unwrap();
        assert!(bad.validate().is_err());
        // High contrast: no fills, no decoration, system colors.
        let white = Rgba { r: 255, g: 255, b: 255, a: 255 };
        let black = Rgba { r: 0, g: 0, b: 0, a: 255 };
        let yellow = Rgba { r: 255, g: 255, b: 0, a: 255 };
        let hc = Theme::graphite().resolve(&SysLook { light: true, high_contrast: Some(HighContrast { window: black, text: white, highlight: yellow, highlight_text: black, gray_text: crate::syslook::GRAY_TEXT }), ..Default::default() });
        assert_eq!(hc.tile().a, 0);
        assert_eq!(hc.wash(0.5).a, 0);
        assert_eq!((hc.active(), hc.on_active(), hc.fill()), (yellow, black, yellow));
        assert_eq!((hc.ink(), hc.text_dim(false), hc.tile_border()), (white, white, white));
        assert!(hc.panel_highlight().is_none() && hc.panel_shadows().is_empty());
    }

    #[test]
    fn resolve_follows_system_and_accessibility() {
        use crate::syslook::{HighContrast, SysLook};
        let t = file_graphite();
        let dark = t.resolve(&SysLook { light: false, ..Default::default() });
        let light = t.resolve(&SysLook { light: true, ..Default::default() });
        assert!(!dark.is_light && light.is_light);
        assert_ne!(dark.dock_tint(), light.dock_tint());
        assert_eq!(light.topbar_text(), over(Rgba { r: 0, g: 0, b: 0, a: 0xE0 }, Rgba { r: 0xF0, g: 0xF0, b: 0xF0, a: 255 }));
        // Transparency off: everything solid.
        let solid = t.resolve(&SysLook { transparency: false, ..Default::default() });
        assert!([&solid.dock, &solid.topbar, &solid.popup].iter().all(|s| s.material == "solid"));
        // High contrast wins over the theme.
        let white = Rgba { r: 255, g: 255, b: 255, a: 255 };
        let black = Rgba { r: 0, g: 0, b: 0, a: 255 };
        let hc = t.resolve(&SysLook { high_contrast: Some(HighContrast { window: black, text: white, highlight: white, highlight_text: black, gray_text: crate::syslook::GRAY_TEXT }), ..Default::default() });
        assert_eq!(hc.topbar_text(), white);
        assert_eq!(hc.dock_tint(), Rgba { a: 255, ..black });
        assert!(hc.dock_shadows(false).is_empty() && hc.dock_matte().is_none() && hc.dock_highlight().is_none(), "no decoration");
        // Transparency off: solid, so no matte slab either.
        assert!(solid.dock_matte().is_none());
        // A fixed mode ignores the system.
        let mut fixed = t.clone();
        fixed.mode = "dark".into();
        assert!(!fixed.resolve(&SysLook { light: true, ..Default::default() }).is_light);
    }

    fn hc_black() -> crate::syslook::HighContrast {
        let (white, black) = (Rgba { r: 255, g: 255, b: 255, a: 255 }, Rgba { r: 0, g: 0, b: 0, a: 255 });
        crate::syslook::HighContrast { window: black, text: white, highlight: Rgba { r: 255, g: 255, b: 0, a: 255 }, highlight_text: black, gray_text: crate::syslook::GRAY_TEXT }
    }

    fn glass_raw() -> Theme {
        let mut t = Theme::graphite();
        t.finish = Finish::Glass;
        t
    }

    /// Graphite as a theme file draws it: its JSON's solid tints, white
    /// see-through cards, E1 to E4 (the base the built-in theme's Flat v2
    /// is laid over: flat_v2_is_the_mockups_plan_2).
    fn file_graphite() -> Theme {
        let mut t = Theme::graphite();
        t.builtin = false;
        t
    }

    #[test]
    fn glass_falls_back_to_flat() {
        use crate::syslook::SysLook;
        // Every glass: Windows' transparency off, high contrast and the
        // blur off (energy saver, the low presets, Low-spec mode, software
        // rendering: glass_blur) are exactly Flat.
        for finish in [Finish::Glass] {
            for light in [false, true] {
                for transparency in [true, false] {
                    for hc in [None, Some(hc_black())] {
                        for blur in [true, false] {
                            let sys = SysLook { light, transparency, high_contrast: hc, ..Default::default() };
                            let mut raw = Theme::graphite();
                            raw.finish = finish;
                            let t = raw.resolve_fx(&sys, blur);
                            let want = transparency && hc.is_none() && blur;
                            assert_eq!(t.glass(), want, "{finish:?} light {light} transparency {transparency} hc {} blur {blur}", hc.is_some());
                            assert_eq!(t.finish == finish, want);
                            assert_eq!(Material::ALL.iter().any(|&m| t.glass_fx(m).is_some()), want, "no effect graph on Flat");
                            // Fallen back: exactly what Flat draws.
                            if !want {
                                let mut flat = Theme::graphite().resolve_fx(&sys, blur);
                                flat.finish = t.finish;
                                flat.chosen = t.chosen;
                                // Forced: no grain (the Flat one chose
                                // keeps it while the blur is on).
                                assert!(!t.flat_grain);
                                flat.flat_grain = false;
                                assert_eq!(t, flat);
                            }
                        }
                    }
                }
            }
        }
        // Flat is never upgraded.
        assert!(!Theme::graphite().resolve_fx(&SysLook::default(), true).glass());
    }

    #[test]
    fn flat_is_todays_look_without_outlines() {
        use crate::syslook::SysLook;
        let raw = file_graphite();
        assert_eq!(raw.finish, Finish::Flat, "the default");
        let lp = raw.light.clone().unwrap();
        for (light, dock, topbar, popup, ui) in [(false, &raw.dock, &raw.topbar, &raw.popup, &raw.ui), (true, &lp.dock, &lp.topbar, &lp.popup, &lp.ui)] {
            let t = raw.resolve_fx(&SysLook { light, ..Default::default() }, true);
            // Graphite has no outlines already: its surfaces as written (the
            // top bar's border token was never drawn; it is cleared too).
            let topbar = Surface { border: "#00000000".into(), ..topbar.clone() };
            assert_eq!((&t.dock, &t.topbar, &t.popup, &t.ui), (dock, &topbar, popup, ui), "light {light}");
            assert_eq!(t.tile_solid(), over(t.tile(), Rgba { a: 255, ..t.popup_tint() }));
            assert_eq!(t.tile_solid().a, 255);
            // The blur effect alone changes nothing on Flat but its grain
            // (Flat v2's; a theme file has none to draw).
            let mut off = raw.resolve_fx(&SysLook { light, ..Default::default() }, false);
            assert!(!off.flat_grain);
            off.flat_grain = t.flat_grain;
            assert_eq!(off, t);
        }
        // A theme with outlines: Flat drops them, nothing else.
        let lined: Theme = serde_json::from_str(r##"{"dock":{"border":"#FFFFFF3C"},"popup":{"border":"#494949"},"ui":{"border":"#FFFFFF1F"}}"##).unwrap();
        let t = lined.resolve_fx(&SysLook::default(), true);
        assert_eq!((t.dock_border().a, t.popup_border().a, t.tile_border().a), (0, 0, 0));
        let mut kept = t.clone();
        kept.dock.border = lined.dock.border.clone();
        kept.popup.border = lined.popup.border.clone();
        kept.topbar.border = lined.topbar.border.clone();
        kept.ui.border = lined.ui.border.clone();
        assert_eq!((&kept.dock, &kept.popup, &kept.ui), (&lined.dock, &lined.popup, &lined.ui));
        // High contrast keeps its edges, in either finish.
        for raw in [lined.clone(), glass_raw()] {
            let hc = raw.resolve_fx(&SysLook { high_contrast: Some(hc_black()), ..Default::default() }, true);
            let white = Rgba { r: 255, g: 255, b: 255, a: 255 };
            assert_eq!((hc.dock_border(), hc.popup_border(), hc.tile_border()), (white, white, white));
        }
    }

    #[test]
    fn glass_tokens_are_sane() {
        use crate::syslook::SysLook;
        let grey = |c: Rgba| c.r == c.g && c.g == c.b;
        for (light, finish) in [false, true].map(|l| (l, Finish::Glass)) {
            let mut raw = Theme::graphite();
            raw.finish = finish;
            let t = raw.resolve_fx(&SysLook { light, ..Default::default() }, true);
            assert!(t.glass() && t.is_light == light);
            // A neutral tint, far thinner than Flat's (opaque): .48 to
            // .66 (the Luminosity layer under it keeps text readable); the
            // top bar a little thinner than the panels.
            let flat = file_graphite().resolve_fx(&SysLook { light, ..Default::default() }, true);
            for (g, f) in [(t.popup_tint(), flat.popup_tint()), (t.dock_tint(), flat.dock_tint()), (t.topbar_tint(), flat.topbar_tint())] {
                assert!(grey(g) && (71..=174).contains(&g.a) && g.a < f.a, "{finish:?} tint {g:?}");
            }
            assert!(t.topbar_tint().a < t.popup_tint().a);
            let coat = t.coat().unwrap();
            // Name labels stay all but opaque in every glass.
            assert!(coat.alpha[Material::Tip as usize] >= 0.94);
            // Cards and tiles: a low-opacity white fill, never glass: 5-8 %
            // on dark; on light as see-through as the plate (final audit 4:
            // under half, where Flat's file tile is 70 %).
            let tile = t.tile();
            assert!((tile.r, tile.g, tile.b) == (255, 255, 255), "{tile:?}");
            let share = tile.a as f32 / 255.0;
            assert!(if light { (0.25..=0.45).contains(&share) } else { (0.05..=0.08).contains(&share) }, "tile {share}");
            // Flat's black shadows (the palette's elevations: no deeper glass
            // ones), the panels' inside their margin unclamped, the dock's
            // inside dock_shadows' range.
            let ps = t.panel_shadows();
            assert_eq!(ps.len(), 2);
            for (x, y, blur, c) in &ps {
                assert!(x.abs().max(y.abs()) + blur <= metrics::SHADOW_M && (c.r, c.g, c.b) == (0, 0, 0));
            }
            assert_eq!(ps.iter().map(|s| (s.1, s.2)).collect::<Vec<_>>(), t.ui.shadows.iter().map(|l| (l.y, l.blur)).collect::<Vec<_>>(), "not clamped");
            assert_eq!(ps, flat.panel_shadows());
            assert_eq!(t.dock_shadows(false), flat.dock_shadows(false));
            let ds = t.dock_shadows(false);
            assert!(ds.iter().all(|s| s.1 <= 32.0 && s.2 <= 64.0 && (s.3.r, s.3.g, s.3.b) == (0, 0, 0)));
            assert_eq!(ds.len(), 2);
            // Tiles: hover and pressed in reach, an opaque look on the panel.
            let (rest, hover, pressed) = t.card_states();
            assert!(rest != hover && hover != pressed);
            assert_eq!(t.tile_solid().a, 255);
            assert!(grey(t.tile_solid()));
            for c in [rest, hover, pressed] {
                assert!(grey(c) && c.a < 255, "a see-through solid fill {c:?}");
            }
            // Text still reads on the panel's own tint (blur on top of it
            // only softens what is under).
            let panel = Rgba { a: 255, ..t.popup_tint() };
            assert!(contrast(t.ink(), panel) >= 4.5 && contrast(t.ink(), over(t.tile(), panel)) >= 4.5, "light {light}");
            // No outlines on tiles: their fill marks them.
            assert_eq!(t.tile_border().a, 0);
        }
    }

    /// Theme::plate_seen's plain plate (the full graph, no top light, no
    /// grain), as the tests below read it on a mid grey.
    fn plate_on(t: &Theme, m: Material, wp: Rgba) -> Rgba {
        t.plate_seen(m, wp)[0]
    }

    fn all_finishes() -> [Finish; 2] {
        [Finish::Flat, Finish::Glass]
    }

    /// Every finish, both palettes, resolved with the blur on.
    fn every_look() -> Vec<Theme> {
        use crate::syslook::SysLook;
        all_finishes()
            .into_iter()
            .flat_map(|finish| {
                [false, true].map(|light| {
                    let mut raw = Theme::graphite();
                    raw.finish = finish;
                    raw.resolve_fx(&SysLook { light, ..Default::default() }, true)
                })
            })
            .collect()
    }

    /// The lowest contrast text keeps on `t` over the backdrops `wps`,
    /// against the whole plate (plate_seen: the top light at its brightest
    /// and the grain at its fullest, every level a brush can fall back
    /// to): (body, secondary), each with where. Body: the ink on the
    /// panels, menus and the dock, the bar's text, the ink on a card or
    /// tile in every state, the primary text on a row (on the panel, on a
    /// card, on the menu). Secondary: on the panel's cards, on a row of the
    /// panel, on the menu and its rows (shortcuts).
    pub(crate) fn worst_text(t: &Theme, wps: &[Rgba]) -> [(f32, String); 2] {
        let read = |text: Rgba, bg: Rgba| contrast(over(text, bg), bg);
        let (rest, hover, pressed) = t.card_states();
        let rows = [State::Hover, State::Pressed, State::Selected].map(|s| t.fill_row(s));
        let mut worst = [(f32::MAX, String::new()), (f32::MAX, String::new())];
        let mut see = |k: usize, c: f32, what: &dyn Fn() -> String| {
            if c < worst[k].0 {
                worst[k] = (c, what());
            }
        };
        for &wp in wps {
            for (m, ink) in [(Material::Panel, t.ink()), (Material::Menu, t.ink()), (Material::Dock, t.ink()), (Material::Bar, t.topbar_text())] {
                for bg in t.plate_seen(m, wp) {
                    see(0, contrast(ink, bg), &|| format!("wp {wp:?} {m:?} on {bg:?}"));
                }
            }
            for panel in t.plate_seen(Material::Panel, wp) {
                for fill in [rest, hover, pressed] {
                    let bg = over(fill, panel);
                    see(0, contrast(t.ink(), bg), &|| format!("wp {wp:?} card {fill:?}"));
                    see(1, read(t.text_secondary(), bg), &|| format!("wp {wp:?} secondary on card {fill:?}"));
                }
                for row in rows {
                    let (on_panel, on_card) = (over(row, panel), over(row, over(rest, panel)));
                    for bg in [on_panel, on_card] {
                        see(0, read(t.text_primary(), bg), &|| format!("wp {wp:?} row {row:?}"));
                    }
                    see(1, read(t.text_secondary(), on_panel), &|| format!("wp {wp:?} secondary on row {row:?}"));
                }
            }
            for menu in t.plate_seen(Material::Menu, wp) {
                for bg in std::iter::once(menu).chain(rows.map(|r| over(r, menu))) {
                    see(0, read(t.text_primary(), bg), &|| format!("wp {wp:?} menu {bg:?}"));
                    see(1, read(t.text_secondary(), bg), &|| format!("wp {wp:?} shortcut {bg:?}"));
                }
            }
        }
        worst
    }

    /// Body and secondary text 4.5:1 on every finish over the backdrops a
    /// glass really shows (BACKDROPS: white, black, a mid grey, a blurred
    /// photo's tones), against the whole plate, on the cards in every state
    /// and on the rows (worst_text). No large-text exception: the bar's and
    /// the menus' text is body size.
    #[test]
    fn glass_text_reads_on_any_wallpaper() {
        for t in every_look() {
            let [(body, at), (secondary, sat)] = worst_text(&t, &BACKDROPS);
            let what = format!("{:?} light {}", t.finish, t.is_light);
            let need = BODY_NEED;
            assert!(body >= need, "{what} {at}: {body:.2}");
            assert!(secondary >= need, "{what} {sat}: {secondary:.2}");
        }
    }

    /// The named exception: over the six full colors (FULL_COLORS) body and
    /// secondary text keep 3:1, not 4.5:1. Holding them to 4.5:1 took the
    /// dark Luminosity layer to .75 to .90 and buried the glass on every
    /// backdrop (glass-v3/results.md, 4). The exception is a real one (some
    /// glass reads below 4.5:1 there) and only there (the photo tones next
    /// to them keep 4.5:1: glass_text_reads_on_any_wallpaper).
    #[test]
    fn full_colors_are_a_3_to_1_exception() {
        assert_eq!(FULL_COLOR_NEED, 3.0);
        let mut below = false;
        for t in every_look() {
            let [(body, at), (secondary, sat)] = worst_text(&t, &FULL_COLORS);
            let what = format!("{:?} light {}", t.finish, t.is_light);
            assert!(body >= FULL_COLOR_NEED, "{what} {at}: {body:.2}");
            assert!(secondary >= FULL_COLOR_NEED, "{what} {sat}: {secondary:.2}");
            below |= body.min(secondary) < BODY_NEED;
            if t.finish == Finish::Flat {
                // Flat is opaque: no exception needed.
                assert!(body.min(secondary) >= BODY_NEED, "{what}");
            }
        }
        assert!(below, "no glass needs the exception: drop it");
        // Every backdrop is one or the other, with its need.
        assert_eq!(backdrops().count(), BACKDROPS.len() + FULL_COLORS.len());
        assert!(backdrops().all(|(c, need)| need == if FULL_COLORS.contains(&c) { FULL_COLOR_NEED } else { BODY_NEED }));
        assert!(!BACKDROPS.iter().any(|c| FULL_COLORS.contains(c)));
    }

    /// Glass v3 review 3: what text is measured against is the plate as
    /// drawn: the top light and the grain lie over the tint, a brush may
    /// fall back, and a full color is not a grey of its lightness.
    #[test]
    fn the_plate_text_sees_is_the_whole_plate() {
        use crate::glass::SPEC_GLOW;
        let white = Rgba { r: 255, g: 255, b: 255, a: 255 };
        for t in every_look() {
            let looks = t.plate_seen(Material::Panel, white);
            let Some(fx) = t.glass_fx(Material::Panel) else {
                // Flat v2: its top and its bottom, each with the grain at
                // its full alpha and without it.
                let (top, bottom) = t.flat_fill(Material::Panel).unwrap();
                let grain = Rgba { r: 128, g: 128, b: 128, a: (t.flat_coat(Material::Panel).unwrap().grain * 255.0).round() as u8 };
                assert_eq!(looks, vec![top, over(grain, top), bottom, over(grain, bottom)]);
                continue;
            };
            // Three levels x (plain, lit) x (no grain, full grain).
            assert_eq!(looks.len(), if fx.noise > 0.0 { 12 } else { 6 });
            // The lit plate is the plain one under the line and the glow.
            let lit = looks[if fx.noise > 0.0 { 2 } else { 1 }];
            let lift = |a: f32| Rgba { r: 255, g: 255, b: 255, a: (a * 255.0).round() as u8 };
            assert_eq!(lit, over(lift(fx.spec * SPEC_GLOW.2), over(lift(fx.spec), looks[0])));
            let l = |c: Rgba| luminance(c);
            if t.is_light {
                // Over white the light can only lift a light plate.
                assert!(l(lit) >= l(looks[0]));
            } else {
                assert!(l(lit) > l(looks[0]), "{:?}: the top light lifts a dark plate", t.finish);
            }
            if fx.noise > 0.0 {
                // The grain pulls the plate toward its mid grey.
                let grained = looks[1];
                assert!((grained.r as i32 - 128).abs() <= (looks[0].r as i32 - 128).abs());
            }
            // The fallback levels: the same lightness over a grey (fold).
            let grey = Rgba { r: 0x80, g: 0x80, b: 0x80, a: 255 };
            let g = t.plate_seen(Material::Panel, grey);
            let step = if fx.noise > 0.0 { 4 } else { 2 };
            for k in 1..3 {
                assert!((g[0].r as i32 - g[k * step].r as i32).abs() <= 2, "{:?} light {}: level {k} {:?} vs {:?}", t.finish, t.is_light, g[k * step], g[0]);
            }
        }
        // A full color is not a grey of its lightness: through the dark
        // Glass's saturation and Luminosity layer some full color comes out
        // lighter than white does, so the greys alone are not the bound.
        let t = every_look().into_iter().find(|t| t.finish == Finish::Glass && !t.is_light).unwrap();
        let plain = |wp: Rgba| t.plate_seen(Material::Panel, wp)[0];
        let whitest = luminance(plain(white));
        assert!(FULL_COLORS.iter().any(|&c| luminance(plain(c)) > whitest), "a full color lighter than white");
    }

    #[test]
    fn readable_raises_only_as_far_as_it_must() {
        let white = Rgba { r: 255, g: 255, b: 255, a: 255 };
        let dim = Rgba { r: 255, g: 255, b: 255, a: 0x9E };
        // On a dark ground the level is kept as it is.
        let dark = Rgba { r: 0x26, g: 0x26, b: 0x26, a: 255 };
        assert_eq!(readable(dim, 0xE6, &[(dark, 4.5)]), dim);
        // On a lighter one it rises to the first alpha that reads, no
        // further (the same as trying every alpha).
        let mid = Rgba { r: 0x5A, g: 0x5A, b: 0x5A, a: 255 };
        let got = readable(dim, 0xE6, &[(dark, 4.5), (mid, 4.5)]);
        let first = |need: f32| (dim.a..=0xE6).find(|&a| contrast(over(Rgba { a, ..dim }, mid), mid) >= need).unwrap();
        assert_eq!(got.a, first(4.5));
        assert!(got.a > dim.a && contrast(over(got, mid), mid) >= 4.5);
        // Each ground with its own need: 3:1 there asks for less.
        let lighter = Rgba { r: 0x70, g: 0x70, b: 0x70, a: 255 };
        let low = readable(dim, 0xE6, &[(dark, 4.5), (lighter, 3.0)]);
        assert!(contrast(over(low, lighter), lighter) >= 3.0 && low.a < readable(dim, 0xE6, &[(lighter, 4.5)]).a);
        // The same ground twice: the higher need holds.
        assert_eq!(readable(dim, 0xE6, &[(mid, 3.0), (mid, 4.5)]), got);
        // Where nothing reads: the cap. Opaque text is kept.
        assert_eq!(readable(dim, 0xE6, &[(white, 4.5)]).a, 0xE6);
        assert_eq!(readable(Rgba { a: 255, ..dim }, 0xE6, &[(white, 4.5)]).a, 255);
    }

    #[test]
    fn glass_is_flat_with_other_tokens() {
        use crate::syslook::SysLook;
        // Nothing drawn over the tint: Glass resolves to Flat's surfaces
        // with only the material, tint, opacity, shadows and fills changed
        // (no highlight, no matte, no decoration of its own).
        for light in [false, true] {
            let sys = SysLook { light, ..Default::default() };
            let g = glass_raw().resolve_fx(&sys, true);
            let mut f = file_graphite().resolve_fx(&sys, true);
            (f.builtin, f.flat_grain) = (g.builtin, g.flat_grain);
            for (fs, gs) in [(&mut f.dock, &g.dock), (&mut f.topbar, &g.topbar), (&mut f.popup, &g.popup)] {
                fs.material = gs.material.clone();
                fs.tint = gs.tint.clone();
                fs.opacity = gs.opacity;
                fs.shadows = gs.shadows.clone();
            }
            assert_eq!(f.ui.tile, g.ui.tile, "the same card fill in both finishes");
            f.ui.menu = g.ui.menu.clone();
            f.ui.tip = g.ui.tip.clone();
            f.ui.dim = g.ui.dim.clone();
            f.finish = g.finish;
            f.chosen = g.chosen;
            // The built-in theme's corners are its finish's; a file keeps its own.
            assert_eq!(g.radii(), Radii::of(Finish::Glass));
            f.ui.radius = g.ui.radius;
            assert_eq!(f, g, "light {light}");
        }
    }

    #[test]
    fn no_gloss_is_drawn_anywhere() {
        // The fake Liquid Glass (a sheen over the panels, the bar and the
        // dock, lit tops on tiles, discs and icons) is gone for good: none
        // of its visuals or tokens is back in the code that draws.
        let sources = [
            include_str!("theme.rs"),
            include_str!("popup.rs"),
            include_str!("dock/mod.rs"),
            include_str!("dock/items.rs"),
            include_str!("dock/preview.rs"),
            include_str!("topbar/mod.rs"),
            include_str!("topbar/control.rs"),
            include_str!("topbar/stats.rs"),
            include_str!("topbar/island/card.rs"),
            include_str!("topbar/island/mod.rs"),
            include_str!("pieces/mod.rs"),
            include_str!("launchpad/view.rs"),
        ];
        let names = ["sheen_", "_sheen", "fn sheen", "SHEEN", "glass_rim", "tile_lit", "icon_lit", "ICON_LIT", "disc_lit", "lit_at_glyph", "lit_v", "tile_top"];
        for src in sources {
            // Whole files (code and tests), but not this list itself.
            let code = src.find("fn no_gloss_is_drawn_anywhere").map_or(src, |i| &src[..i]);
            for n in names {
                assert!(!code.contains(n), "{n} is back");
            }
        }
    }

    #[test]
    fn software_rendering_counts_as_blur_off() {
        use crate::syslook::SysLook;
        assert!(glass_blur(true, false));
        assert!(!glass_blur(true, true), "WARP: no Glass");
        assert!(!glass_blur(false, false) && !glass_blur(false, true));
        // On WARP (chosen or a recovery's fallback) Glass is exactly Flat;
        // back on the hardware device, Glass again (resolved anew from the
        // raw theme each time, gpu.rs).
        for light in [false, true] {
            let sys = SysLook { light, ..Default::default() };
            let warp = glass_raw().resolve_fx(&sys, glass_blur(true, true));
            let mut flat = Theme::graphite().resolve_fx(&sys, true);
            flat.finish = warp.finish;
            flat.chosen = warp.chosen;
            // A Flat the PC forces: the same, but its grain.
            assert!(flat.flat_grain && !warp.flat_grain);
            flat.flat_grain = false;
            assert_eq!(warp, flat);
            assert!(glass_raw().resolve_fx(&sys, glass_blur(true, false)).glass());
        }
    }

    #[test]
    fn glass_draws_no_lines() {
        use crate::syslook::SysLook;
        // The owner: no lines. No edge stroke on any surface, no 1 DIP top
        // line, nothing over the tint; depth is the tint and the shadows.
        for light in [false, true] {
            let t = glass_raw().resolve_fx(&SysLook { light, ..Default::default() }, true);
            assert!(t.glass());
            for s in [&t.dock, &t.topbar, &t.popup] {
                assert_eq!(parse_color(&s.border).map(|c| c.a), Some(0), "edge {:?} (light {light})", s.border);
                assert!(s.highlight.is_empty(), "top line {:?}", s.highlight);
            }
            assert_eq!((t.dock_border().a, t.popup_border().a, t.tile_border().a), (0, 0, 0));
            assert!(t.panel_highlight().is_none() && t.dock_highlight().is_none(), "no top line (light {light})");
            assert!(!t.dock_shadows(false).is_empty() && !t.panel_shadows().is_empty(), "and the shadows");
        }
        // High contrast keeps its outlines.
        let hc = glass_raw().resolve_fx(&SysLook { high_contrast: Some(hc_black()), ..Default::default() }, true);
        let white = Rgba { r: 255, g: 255, b: 255, a: 255 };
        assert_eq!((hc.dock_border(), hc.popup_border(), hc.tile_border()), (white, white, white));
        assert!(!hc.glass());
    }

    #[test]
    fn type_ramp_is_the_owners_and_climbs() {
        use ramp::*;
        // Caption 11/14, Footnote 12/16, Body 13/18 (+ strong), Headline
        // 15/20, Title 20/26, Large title 26/32; 400 and 600 only.
        let want = [(11.0, 14.0, 400), (12.0, 16.0, 400), (13.0, 18.0, 400), (13.0, 18.0, 600), (15.0, 20.0, 600), (20.0, 26.0, 600), (26.0, 32.0, 600)];
        assert_eq!(RAMP.map(|t| (t.size, t.line, t.weight)), want);
        for w in RAMP.windows(2) {
            assert!(w[1].size >= w[0].size && w[1].line >= w[0].line, "{w:?}");
            assert!(w[1].size > w[0].size || w[1].weight > w[0].weight, "a step up in size or weight: {w:?}");
        }
        for t in RAMP.iter().chain([&DISPLAY]) {
            assert!(t.weight == REGULAR || t.weight == SEMIBOLD, "never Bold: {t:?}");
            assert!(t.line > t.size && t.size >= 11.0, "{t:?}");
        }
        assert!(DISPLAY.size > LARGE_TITLE.size, "the clock is the exception");
        // The old names are the ramp's sizes; the base text is 13.
        assert_eq!((metrics::CAPTION, metrics::SMALL, metrics::BODY), (11.0, 12.0, 13.0));
        assert_eq!(BODY.at(1.5), (19.5, 27.0));
    }

    #[test]
    fn spacing_and_radii_are_the_scale() {
        use metrics::space::*;
        assert_eq!(SCALE, [2.0, 4.0, 8.0, 12.0, 16.0, 24.0, 32.0]);
        assert!(SCALE.windows(2).all(|w| w[1] > w[0]));
        assert!(SCALE[1..].iter().all(|v| v % 4.0 == 0.0), "the 4 DIP grid (2 only for hairline gaps)");
        // A panel's two lines (3.3): its cards 8 in, their content 12
        // further.
        assert_eq!((metrics::PANEL_PAD, metrics::CARD_PAD), (S3, S4));
        // Radii: sheet 28, panel 20, menu 12, card / tile 12, control 8,
        // tooltip 8, inner 4.
        let r = Radii::default();
        assert_eq!((r.sheet, r.panel, r.menu, r.tile, r.card, r.control, r.tip, r.inner), (28.0, 20.0, 12.0, 12.0, 12.0, 8.0, 8.0, 4.0));
        assert_eq!(Radii::light(), r, "the same in both modes");
        // Concentric, all the way down: a card in a panel's 8 DIP padding,
        // a control in a card's 4, a thumb in a control's 2 (never under
        // the inner radius), a menu row inset 4, a sheet's inner 12.
        assert_eq!(concentric(r.panel, metrics::PANEL_PAD), r.card);
        assert_eq!(concentric(r.card, S2), r.control);
        assert_eq!(concentric(r.control, S1), 6.0);
        assert_eq!(concentric(r.menu, metrics::size::MENU_PAD), r.control);
        assert_eq!(concentric(r.sheet, S5), 12.0);
        // Never under 4: at 0 a corner stands square.
        assert_eq!(concentric(4.0, 8.0), r.inner);
        assert_eq!(concentric(r.control, S3), INNER_RADIUS);
        assert_eq!((capsule(28.0), capsule(-2.0)), (14.0, 0.0));
        // The dock: 20 in both modes.
        let t = Theme::graphite();
        let lp = t.light.as_ref().unwrap();
        assert_eq!((t.dock.radius, lp.dock.radius), (20.0, 20.0));
        // A theme's wild new radii are kept sane too.
        let wild: Theme = serde_json::from_str(r##"{"ui":{"radius":{"menu":99,"tip":-1,"sheet":500}}}"##).unwrap();
        assert_eq!((wild.radii().menu, wild.radii().tip, wild.radii().sheet), (32.0, 0.0, 32.0));
    }

    #[test]
    fn part_sizes_are_the_spec_and_sit_on_the_grid() {
        use metrics::size::*;
        // Rows by what they hold; the glyph in each is about half as tall.
        assert_eq!([ROW_S, ROW_M, ROW_L, ROW_XL, ROW_2XL], [24.0, 28.0, 32.0, 40.0, 48.0]);
        assert_eq!((HEAD_H, CONTROL_H, BUTTON_WIDE_H, BUTTON_PAD_X), (24.0, 28.0, 32.0, 12.0));
        assert_eq!((TOGGLE_W, TOGGLE_H, TOGGLE_KNOB), (36.0, 20.0, 16.0));
        assert_eq!((TRACK_H, KNOB, KNOB_HELD), (4.0, 16.0, 1.12));
        assert_eq!((EDGE_GAP, FOCUS_W, FOCUS_GAP, SCROLL_FADE), (8.0, 2.0, 2.0, 12.0));
        assert_eq!((DOCK_GAP, BAR_INSET, SEP_H, MENU_PAD), (8.0, 2.0, 8.0, 4.0));
        assert_eq!((CHECK, CHECK_R, CHECK_STROKE), (16.0, 4.0, 1.5));
        assert_eq!((FIELD_L, ART, ART_R), (32.0, 56.0, 8.0));
        assert_eq!((CLOSE, CLOSE_GLYPH, CLOSE_INSET), (24.0, 12.0, 8.0));
        assert_eq!((QUIET_MIN_H, SPINNER, SPINNER_STROKE), (96.0, 16.0, 1.5));
        assert_eq!([ICON_XS, ICON_S, ICON_M, ICON_L], [12.0, 16.0, 20.0, 24.0]);
        assert_eq!((PANEL_PAD, CARD_PAD), (8.0, 12.0));
        // The content line is the container line + 12: 20 in a panel, 16
        // in a menu.
        assert_eq!((PANEL_PAD + CARD_PAD, MENU_PAD + CARD_PAD), (20.0, 16.0));
        // Sizes that are gaps or heights sit on the 4 DIP grid.
        for v in [ROW_S, ROW_M, ROW_L, ROW_XL, ROW_2XL, HEAD_H, CONTROL_H, BUTTON_WIDE_H, BUTTON_PAD_X, TOGGLE_W, TOGGLE_H, TOGGLE_KNOB, TRACK_H, KNOB, EDGE_GAP, SCROLL_FADE, DOCK_GAP, SEP_H, CHECK, FIELD_L, ART, CLOSE, QUIET_MIN_H, SPINNER] {
            assert_eq!(v % 4.0, 0.0, "{v}");
        }
        // A capsule in the bar is the bar less 4; a 16 glyph button is a
        // 28 capsule (at least 24 to press).
        assert_eq!(32.0 - BAR_INSET * 2.0, CONTROL_H);
        assert!(CLOSE >= 24.0 && CONTROL_H >= 24.0);
    }

    #[test]
    fn elevations_are_two_soft_black_layers_inside_the_margin() {
        use crate::syslook::SysLook;
        // (near y / blur / light / dark), (far ...): the spec's table (2.3).
        let table = [
            (Elevation::E1, (1.0, 2.0, 0.10, 0.28), (2.0, 6.0, 0.08, 0.22)),
            (Elevation::E2, (3.0, 8.0, 0.09, 0.28), (10.0, 24.0, 0.12, 0.30)),
            (Elevation::E3, (6.0, 14.0, 0.10, 0.30), (16.0, 32.0, 0.14, 0.34)),
            (Elevation::E4, (8.0, 18.0, 0.12, 0.34), (20.0, 40.0, 0.16, 0.36)),
        ];
        for light in [false, true] {
            let t = file_graphite().resolve(&SysLook { light, ..Default::default() });
            let mut last = (0.0, 0.0);
            for (e, near, far) in table {
                let [(ny, nb, na), (fy, fb, fa)] = e.layers(light);
                assert_eq!((ny, nb, na), (near.0, near.1, if light { near.2 } else { near.3 }), "{e:?} near");
                assert_eq!((fy, fb, fa), (far.0, far.1, if light { far.2 } else { far.3 }), "{e:?} far");
                // Near: small and close. Far: wider and further (the air
                // under a floating thing), not a ring stuck to its edge.
                assert!(fy > ny && fb > nb * 2.0, "{e:?}");
                // One light, above: every shadow falls straight down.
                assert!(ny > 0.0 && fy > 0.0);
                assert!(fb > last.0 && fy > last.1, "each level higher than the last");
                last = (fb, fy);
                let drawn = t.elevation(e);
                assert_eq!(drawn.len(), 2);
                for (x, y, blur, c) in &drawn {
                    assert!((c.r, c.g, c.b) == (0, 0, 0) && c.a > 0, "black: {c:?}");
                    assert_eq!(*x, 0.0, "{e:?}: no sideways shadow");
                    if e != Elevation::E4 {
                        // Panels' and menus' windows have SHADOW_M around them.
                        assert!(y.abs() + blur <= metrics::SHADOW_M, "{e:?} inside SHADOW_M");
                    }
                }
                let ramp: Vec<(f32, f32, u8)> = e.layers(light).iter().map(|&(y, b, a)| (y, b, (a * 255.0).round() as u8)).collect();
                assert_eq!(drawn.iter().map(|s| (s.1, s.2, s.3.a)).collect::<Vec<_>>(), ramp, "{e:?} as the ramp says");
            }
            // The JSON's panel and dock shadows are E3 and E4, unclamped.
            assert_eq!(t.panel_shadows(), t.elevation(Elevation::E3));
            assert_eq!(t.dock_shadows(false), t.elevation(Elevation::E4));
            assert_eq!(t.dock_shadows(true), t.elevation(Elevation::E4), "blur off too");
            assert_eq!(t.panel_shadows().iter().map(|s| (s.1, s.2)).collect::<Vec<_>>(), vec![(6.0, 14.0), (16.0, 32.0)]);
        }
        // The panels' room is exactly E3's far shadow: 16 down + 32 of blur.
        assert_eq!(metrics::SHADOW_M, 48.0);
        // (The room is either finish's: Flat v2's name labels and menus
        // reach further, FLAT.)
        assert_eq!(Elevation::ALL.map(|e| e.reach()), [8.0, 34.0, 48.0, 60.0]);
        let hc = Theme::graphite().resolve(&SysLook { high_contrast: Some(hc_black()), ..Default::default() });
        assert!(Elevation::ALL.iter().all(|&e| hc.elevation(e).is_empty()), "none in high contrast");
    }

    #[test]
    fn light_palette_is_neutral() {
        use crate::syslook::SysLook;
        let grey = |c: Rgba| c.r == c.g && c.g == c.b;
        let t = Theme::graphite();
        let lp = t.light.as_ref().unwrap();
        let mut colors: Vec<String> = vec![lp.dock.tint.clone(), lp.topbar.tint.clone(), lp.popup.tint.clone(), lp.indicator.color.clone()];
        colors.extend(lp.dock.matte.iter().map(|m| m.tint.clone()));
        colors.extend(lp.ui.colors().iter().map(|(_, c)| c.to_string()));
        colors.extend(lp.ui.shadows.iter().chain(lp.dock.shadows.iter()).map(|l| l.color.clone()));
        colors.extend([lp.ui.menu.clone(), lp.ui.tip.clone(), hex8(GLASS_DIM_LIGHT)]);
        colors.extend(GLASS_GREY_LIGHT.map(grey_hex));
        for c in &colors {
            assert!(grey(parse_color(c).unwrap()), "{c} has a hue");
        }
        // And what the code draws with on light: text, states, fallbacks.
        for finish in all_finishes() {
            for transparency in [true, false] {
                let mut raw = Theme::graphite();
                raw.finish = finish;
                let l = raw.resolve(&SysLook { light: true, transparency, ..Default::default() });
                let (rest, hover, pressed) = l.card_states();
                for c in [l.ink(), l.text_dim(false), l.text_dim(true), l.text_primary(), l.text_secondary(), l.text_tertiary(), l.text_disabled(), l.dock_tint(), l.topbar_tint(), l.popup_tint(), l.topbar_text(), rest, hover, pressed, l.track(), l.hover(false), l.label_colors().0] {
                    assert!(grey(c), "{c:?} ({finish:?}, transparency {transparency})");
                }
            }
        }
    }

    #[test]
    fn text_is_the_opacity_levels() {
        use crate::syslook::SysLook;
        let byte = |v: f32| (v * 255.0).round() as u8;
        let t = Theme::graphite();
        for (u, lv, ink) in [(&t.ui, TEXT_DARK, 255u8), (&t.light.as_ref().unwrap().ui, TEXT_LIGHT, 0u8)] {
            for (c, level) in [(&u.text, lv.primary), (&u.dim, lv.secondary), (&u.faint, lv.tertiary)] {
                assert_eq!(parse_color(c), Some(Rgba { r: ink, g: ink, b: ink, a: byte(level) }), "{c}");
            }
        }
        assert_eq!((TEXT_DARK.primary, TEXT_DARK.secondary, TEXT_DARK.tertiary, TEXT_DARK.disabled), (0.90, 0.62, 0.42, 0.30));
        assert_eq!((TEXT_LIGHT.primary, TEXT_LIGHT.secondary, TEXT_LIGHT.tertiary, TEXT_LIGHT.disabled), (0.88, 0.60, 0.45, 0.30));
        for light in [false, true] {
            let r = t.resolve(&SysLook { light, ..Default::default() });
            // Levels fall: primary > secondary > tertiary, and disabled
            // under the secondary (raised from its level only as far as
            // DISABLED_NEED asks: disabled_text_reads_on_every_finish).
            let a = [r.text_primary(), r.text_secondary(), r.text_tertiary()].map(|c| c.a);
            assert!(a.windows(2).all(|w| w[0] > w[1]), "{a:?}");
            assert!(r.text_disabled().a >= byte(0.30) && r.text_disabled().a < r.text_secondary().a, "{:?}", r.text_disabled());
            // The opaque accessors are the levels on the menus' surface.
            let menu = Rgba { a: 255, ..r.popup_tint() };
            assert_eq!(r.ink(), over(r.text_primary(), menu));
            assert_eq!(r.text_dim(false), over(r.text_secondary(), menu));
            assert_eq!(r.text_dim(true), over(r.text_tertiary(), menu));
            assert!([r.ink(), r.text_dim(false), r.text_dim(true)].iter().all(|c| c.a == 255));
        }
        // A theme's own opaque text stays as written.
        let mine: Theme = serde_json::from_str(r##"{"ui":{"text":"#C0C0C0"}}"##).unwrap();
        assert_eq!(mine.ink(), Rgba { r: 0xC0, g: 0xC0, b: 0xC0, a: 255 });
        // High contrast: WindowText everywhere but what cannot be used:
        // GrayText (states-issues 3: a disabled row was told from nothing).
        let hc = t.resolve(&SysLook { high_contrast: Some(hc_black()), ..Default::default() });
        let white = Rgba { r: 255, g: 255, b: 255, a: 255 };
        assert_eq!([hc.text_primary(), hc.text_secondary(), hc.text_tertiary()], [white; 3]);
        assert_eq!(hc.text_disabled(), crate::syslook::GRAY_TEXT);
        assert_eq!(hc.text_of(Level::Primary, State::Disabled), crate::syslook::GRAY_TEXT);
        assert_ne!(hc.text_disabled(), hc.text_primary());
    }

    /// Final audit 4: on a light glass the cards (the calendar's events,
    /// the control center's tiles, the PC's state) were near opaque white
    /// tiles floating on the see-through plate. They are white still, at
    /// fills::GLASS_CARD_LIGHT: lighter than the plate, see-through like
    /// it, hover over rest over pressed. Flat, the dark palette and high
    /// contrast keep theirs.
    /// states-issues 6 (2026-11-11): the calendar's event cards lie on a
    /// menu (M3), a step lighter than the panels (M2) whose card Flat v2's
    /// card is: on dark the card was the menu's very grey. On Flat, a
    /// visible step over the menu's top on both palettes; elsewhere the
    /// tile as before.
    #[test]
    fn a_card_on_a_menu_parts_from_it() {
        use crate::syslook::SysLook;
        for light in [false, true] {
            let t = Theme::graphite().resolve_fx(&SysLook { light, ..Default::default() }, true);
            let (menu, _) = t.flat_fill(Material::Menu).unwrap();
            let card = t.menu_card();
            assert!(card.r.abs_diff(menu.r) >= 6 && card.a == 255, "light {light}: menu {menu:?} card {card:?}");
            let mut g = Theme::graphite();
            g.finish = Finish::Glass;
            let g = g.resolve_fx(&SysLook { light, ..Default::default() }, true);
            assert_eq!(g.menu_card(), g.fill_tile(State::Rest));
        }
    }

    #[test]
    fn light_glass_cards_are_see_through() {
        use crate::syslook::SysLook;
        for finish in [Finish::Glass] {
            let mut raw = Theme::graphite();
            raw.finish = finish;
            let t = raw.resolve_fx(&SysLook { light: true, ..Default::default() }, true);
            let (rest, hover, pressed) = t.card_states();
            for c in [rest, hover, pressed] {
                assert!((c.r, c.g, c.b) == (255, 255, 255) && c.a < 140, "{finish:?} {c:?}");
            }
            assert!(hover.a > rest.a && rest.a > pressed.a, "{finish:?}");
            assert_eq!(t.tile(), rest);
            // Lighter than the plate over a mid grey, not a white tile.
            let plate = over(t.material(Material::Panel), Rgba { r: 0x80, g: 0x80, b: 0x80, a: 255 });
            let card = over(rest, plate);
            assert!(card.r > plate.r && card.r < 0xF0, "{finish:?}: {plate:?} -> {card:?}");
            // The dark palette and Flat: as they were.
            let d = raw.resolve_fx(&SysLook { light: false, ..Default::default() }, true);
            assert_eq!(d.tile().a, (fills::CARD_REST.0 * 255.0).round() as u8);
        }
        let flat = Theme::graphite().resolve_fx(&SysLook { light: true, ..Default::default() }, true);
        assert!(flat.card_states().0.a == 255, "Flat v2's opaque cards");
    }

    #[test]
    fn disabled_text_reads_on_every_finish() {
        use crate::syslook::SysLook;
        // A row that cannot be used ('날짜순' in the audit's menu) read
        // 1.5:1 on 0.1.63's Clear and 2.1 on Flat: DISABLED_NEED on the
        // panels' and the menus' plates over the neutral greys, on every
        // finish, and still dimmer than the secondary text.
        for finish in all_finishes() {
            for light in [false, true] {
                let mut raw = Theme::graphite();
                raw.finish = finish;
                let t = raw.resolve_fx(&SysLook { light, ..Default::default() }, true);
                let off = t.text_disabled();
                assert!(off.a < t.text_secondary().a, "{finish:?} light {light}: {off:?}");
                for m in [Material::Panel, Material::Menu] {
                    for g in DISABLED_GREYS {
                        for p in t.plate_seen(m, Rgba { r: g, g, b: g, a: 255 }) {
                            let k = contrast(over(off, p), p);
                            assert!(k >= DISABLED_NEED, "{finish:?} light {light} {m:?} over #{g:02X}: {k:.2} (alpha {})", off.a);
                        }
                    }
                }
                eprintln!("{finish:?} light {light}: disabled alpha {:.2}", off.a as f32 / 255.0);
            }
        }
    }

    #[test]
    fn state_fills_are_the_tokens() {
        use crate::syslook::SysLook;
        let t = file_graphite();
        let byte = |v: f32| (v * 255.0).round() as u8;
        for (light, ink) in [(false, 255u8), (true, 0u8)] {
            let r = t.resolve(&SysLook { light, ..Default::default() });
            let pick = |(d, l): (f32, f32)| if light { l } else { d };
            let wash = |a: f32| Rgba { r: ink, g: ink, b: ink, a: byte(a) };
            assert_eq!(r.fill_row(State::Rest).a, 0);
            assert_eq!(r.fill_row(State::Hover), wash(pick(fills::ROW_HOVER)));
            assert_eq!(r.fill_row(State::Pressed), wash(pick(fills::ROW_PRESSED)));
            assert_eq!(r.fill_row(State::Selected), wash(pick(fills::ROW_SELECTED)));
            // Today's hover() is the row's.
            assert_eq!((r.hover(false), r.hover(true)), (r.fill_row(State::Hover), r.fill_row(State::Pressed)));
            let rows = [State::Rest, State::Hover, State::Pressed, State::Selected].map(|s| r.fill_row(s).a);
            assert!(rows.windows(2).all(|w| w[1] > w[0]), "{rows:?}");
            // Cards: white, lighter hovered, under their rest pressed (2.4).
            let card = |a: f32| Rgba { r: 255, g: 255, b: 255, a: byte(a) };
            assert_eq!(r.fill_tile(State::Rest), card(pick(fills::CARD_REST)));
            assert_eq!(r.fill_tile(State::Hover), card(pick(fills::CARD_HOVER)));
            assert_eq!(r.fill_tile(State::Pressed), card(pick(fills::CARD_PRESSED)));
            let tiles = [State::Pressed, State::Rest, State::Hover].map(|s| r.fill_tile(s).a);
            assert!(tiles.windows(2).all(|w| w[1] > w[0]), "{tiles:?}");
            assert_eq!(r.fill_tile(State::Selected), r.active());
            assert_eq!(r.fill_track(), wash(pick(fills::TRACK)));
            // The theme's tile is the rest fill; the opaque one is it on
            // the panels' surface, the track likewise.
            let menu = Rgba { a: 255, ..r.popup_tint() };
            assert_eq!(r.tile(), r.fill_tile(State::Rest));
            assert_eq!(r.tile_solid(), over(r.tile(), menu));
            assert_eq!(r.card_states(), (r.fill_tile(State::Rest), r.fill_tile(State::Hover), r.fill_tile(State::Pressed)));
            assert_eq!(r.track(), over(r.fill_track(), menu));
            // One separator: the hairline (.08); the dock's is .16.
            assert_eq!((r.hairline(), r.separator(), r.dock_separator()), (wash(fills::HAIRLINE), wash(fills::HAIRLINE), wash(fills::DOCK_SEPARATOR)));
        }
        // High contrast: no fills.
        let hc = t.resolve(&SysLook { high_contrast: Some(hc_black()), ..Default::default() });
        for s in [State::Hover, State::Pressed, State::Selected] {
            assert_eq!(hc.fill_row(s).a, 0);
        }
        assert_eq!(hc.fill_tile(State::Hover).a, 0);
    }

    #[test]
    fn text_reads_on_flat_and_glass_over_any_wallpaper() {
        use crate::syslook::SysLook;
        let (white, grey, black) = (Rgba { r: 255, g: 255, b: 255, a: 255 }, Rgba { r: 0x80, g: 0x80, b: 0x80, a: 255 }, Rgba { r: 0, g: 0, b: 0, a: 255 });
        for finish in all_finishes() {
            for light in [false, true] {
                let mut raw = Theme::graphite();
                raw.finish = finish;
                let t = raw.resolve_fx(&SysLook { light, ..Default::default() }, true);
                assert_eq!(t.finish, finish);
                let what = format!("{finish:?} light {light}");
                // Translucent text over a plate over the wallpaper
                // (plate_on).
                let read = |text: Rgba, bg: Rgba| contrast(over(text, bg), bg);
                let body = 4.5;
                // A mid-grey wallpaper (glass v3's bar: however thin the
                // plate, body text 4.5:1 and large text 3:1 on every
                // surface): primary and secondary text are body text,
                // tertiary is large or secondary only (3:1 where it is
                // used, panels and menus).
                for m in Material::ALL {
                    let bg = plate_on(&t, m, grey);
                    assert!(read(t.text_primary(), bg) >= body, "{what} {m:?} primary on grey: {:.2}", read(t.text_primary(), bg));
                    assert!(read(t.text_secondary(), bg) >= body, "{what} {m:?} secondary on grey: {:.2}", read(t.text_secondary(), bg));
                    if matches!(m, Material::Panel | Material::Menu) {
                        assert!(read(t.text_tertiary(), bg) >= 3.0, "{what} {m:?} tertiary on grey: {:.2}", read(t.text_tertiary(), bg));
                    }
                }
                // White and black, the extremes (glass_text_reads_on_any_
                // wallpaper has the full colors, cards and rows): primary
                // and secondary text 4.5:1 on every finish, against every
                // look of the plate.
                for wp in [white, black] {
                    for m in [Material::Bar, Material::Dock, Material::Panel, Material::Menu] {
                        for bg in t.plate_seen(m, wp) {
                            assert!(read(t.text_primary(), bg) >= body, "{what} {m:?} primary on {wp:?}: {:.2}", read(t.text_primary(), bg));
                            if matches!(m, Material::Panel | Material::Menu) {
                                assert!(read(t.text_secondary(), bg) >= body, "{what} {m:?} secondary on {wp:?}: {:.2}", read(t.text_secondary(), bg));
                            }
                        }
                    }
                    if finish == Finish::Flat {
                        // Flat is opaque: the wallpaper does not matter.
                        let p = plate_on(&t, Material::Panel, wp);
                        assert!(read(t.text_secondary(), p) >= 4.5 && contrast(t.ink(), p) >= 4.5, "{what}");
                    }
                }
                // The opaque accessors over a mid grey: ink() on the
                // panels; text_dim() where it is drawn (the island's card
                // on the labels' material, the settings window and notes
                // on Flat: a glass panel draws the translucent levels,
                // popup's own test).
                let dim = contrast(t.text_dim(false), plate_on(&t, Material::Tip, grey));
                assert!(dim >= 4.5, "{what} text_dim on grey: {dim:.2}");
                assert!(contrast(t.ink(), plate_on(&t, Material::Panel, grey)) >= 4.5, "{what} ink on grey");
                // Tertiary: large or secondary text only (3:1 on the menus).
                assert!(contrast(t.text_dim(true), Rgba { a: 255, ..t.popup_tint() }) >= 3.0, "{what} tertiary");
            }
        }
    }

    #[test]
    fn bad_theme_falls_back() {
        let dir = std::env::temp_dir().join(format!("nopbar-theme-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("themes")).unwrap();
        std::fs::write(dir.join("themes").join("bad.json"), r#"{"dock":{"tint":"red"}}"#).unwrap();
        assert_eq!(Theme::load("file:themes\\bad.json", &dir).name, "Graphite");
        assert_eq!(Theme::load("builtin:nope", &dir).name, "Graphite");
        assert_eq!(Theme::load("garbage", &dir).name, "Graphite");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn user_theme_loads() {
        let dir = std::env::temp_dir().join(format!("nopbar-theme-ok-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("themes")).unwrap();
        std::fs::write(dir.join("themes").join("mine.json"), r##"{"name":"Mine","dock":{"tint":"#102030","opacity":0.5}}"##).unwrap();
        let t = Theme::load("file:themes\\mine.json", &dir);
        assert_eq!(t.name, "Mine");
        assert_eq!(t.dock_tint(), Rgba { r: 0x10, g: 0x20, b: 0x30, a: 128 });
        // A theme from before shadows: a flat bar, as it was.
        assert!(t.dock_shadows(false).is_empty() && t.dock_matte().is_none());
        // A broken shadow color is a broken theme.
        std::fs::write(dir.join("themes").join("bad-shadow.json"), r##"{"name":"S","dock":{"shadows":[{"y":2,"color":"black"}]}}"##).unwrap();
        assert_eq!(Theme::load("file:themes\\bad-shadow.json", &dir).name, "Graphite");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn flat_v2_is_the_mockups_plan_2() {
        use crate::syslook::SysLook;
        let grey = |v: u8| Rgba { r: v, g: v, b: v, a: 255 };
        let byte = |a: f32| (a * 255.0).round() as u8;
        for light in [false, true] {
            let sys = SysLook { light, ..Default::default() };
            let t = Theme::graphite().resolve_fx(&sys, true);
            assert_eq!((t.finish, t.flat_grain), (Finish::Flat, true), "the Flat one chose, the blur on: grain");
            let coats = &FLAT[light as usize];
            let mut last = 0.0;
            for m in Material::ALL {
                let c = &coats[m as usize];
                // One grey per plate (paper since 2026-11-12: the light
                // from above is Glass's).
                assert_eq!(t.flat_fill(m), Some((grey(c.top), grey(c.bottom))), "{m:?} light {light}");
                assert_eq!(c.top, c.bottom, "{m:?}: one grey");
                // One color where one is drawn: the mean, opaque.
                assert_eq!(t.material(m), grey(c.mean()), "{m:?}");
                assert_eq!(t.flat_grain(m), c.grain);
                // No rim, no edge light (all clear), no grain.
                assert_eq!(t.rim_of(m).map(|r| r.map(|w| w.a)), Some([0; 3]), "{m:?}");
                assert_eq!(c.grain, 0.0, "{m:?}");
                // Each level lighter than the one under it (L*, its mean).
                let l = luminance(grey(c.mean()));
                assert!(l > last, "{m:?} steps up");
                last = l;
            }
            assert_eq!(t.rim(), t.rim_of(Material::Panel));
            // Its own shadows (glass keeps E1 to E4), inside their room.
            for e in Elevation::ALL {
                let c = &coats[e.material() as usize];
                let want: Vec<_> = [c.near, c.far].iter().map(|&(y, b, a)| (0.0, y, b, Rgba { r: 0, g: 0, b: 0, a: byte(a) })).collect();
                assert_eq!(t.elevation(e), want, "{e:?}");
                assert!(c.near.0 + c.near.1 <= e.reach() && c.far.0 + c.far.1 <= e.reach(), "{e:?} fits its room");
            }
            assert!(Elevation::E3.reach() <= metrics::SHADOW_M);
            assert_eq!(t.panel_shadows(), t.elevation(Elevation::E3));
            assert_eq!(t.dock_shadows(false), t.elevation(Elevation::E4));
            assert_eq!(t.contact().a, byte(coats[2].contact));
            // Cards opaque, a step over the panel; the light and the drop.
            let card = FLAT_CARD[light as usize];
            assert_eq!(t.card_states(), (grey(card.rest), grey(card.hover), grey(card.pressed)));
            assert_eq!(t.tile(), grey(card.rest));
            assert!(card.rest > coats[2].top && card.hover > card.rest && card.pressed < card.rest);
            // Each state a visible step from rest (states-issues: light
            // hover was 2 levels, unseen): 6 levels at least.
            assert!(card.hover - card.rest >= 6 && card.rest - card.pressed >= 6, "light {light}: {card:?}");
            assert_eq!(t.card_light(), Some((Rgba { r: 255, g: 255, b: 255, a: byte(card.light) }, Rgba { r: 0, g: 0, b: 0, a: byte(card.drop) })));
            // The bar's solid band is M0's fall in any finish.
            assert_eq!(t.bar_band(), t.flat_fill(Material::Bar));
            let g = glass_raw().resolve_fx(&sys, true);
            assert_eq!(g.bar_band(), t.bar_band());
            // A glass has none of it: its own plates, E1 to E4, see-through cards.
            assert!(Material::ALL.iter().all(|&m| g.flat_fill(m).is_none() && g.flat_grain(m) == 0.0));
            assert!(g.card_light().is_none());
            for e in Elevation::ALL {
                let want: Vec<_> = e.layers(light).iter().map(|&(y, b, a)| (0.0, y, b, Rgba { r: 0, g: 0, b: 0, a: byte(a) })).collect();
                assert_eq!(g.elevation(e), want, "glass {e:?}");
            }
            // A Flat the PC forces: the same plates, no grain. Windows'
            // transparency off forces it too, on the Flat one chose as on a
            // glass (review 1: it is one of the forcing conditions).
            for (finish, blur, transparency) in [(Finish::Glass, false, true), (Finish::Glass, true, false), (Finish::Flat, false, true), (Finish::Flat, true, false), (Finish::Flat, false, false)] {
                let mut raw = Theme::graphite();
                raw.finish = finish;
                let f = raw.resolve_fx(&SysLook { light, transparency, ..Default::default() }, blur);
                assert_eq!(f.finish, Finish::Flat);
                assert!(Material::ALL.iter().all(|&m| f.flat_grain(m) == 0.0 && f.flat_fill(m) == t.flat_fill(m)), "{finish:?} blur {blur} transparency {transparency}");
                assert_eq!((f.elevation(Elevation::E2), f.card_states()), (t.elevation(Elevation::E2), t.card_states()));
            }
            // The Flat one chose loses its grain with transparency off, and
            // the theme says so (the surfaces drawn with it are drawn again).
            let off = Theme::graphite().resolve_fx(&SysLook { light, transparency: false, ..Default::default() }, true);
            assert!(Material::ALL.iter().all(|&m| off.flat_grain(m) == 0.0));
            assert!(!off.flat_grain && off != t);
            // The rule itself: chosen Flat, and nothing forcing.
            for (blur, transparency, hc) in [(true, true, false), (false, true, false), (true, false, false), (true, true, true)] {
                assert_eq!(flat_grain_on(Finish::Flat, blur, transparency, hc), blur && transparency && !hc);
                for f in [Finish::Glass] {
                    assert!(!flat_grain_on(f, blur, transparency, hc), "{f:?}: a glass fallen to Flat has no grain");
                }
            }
            // High contrast: its own rules, none of this.
            let hc = Theme::graphite().resolve_fx(&SysLook { light, high_contrast: Some(hc_black()), ..Default::default() }, true);
            assert!(Material::ALL.iter().all(|&m| hc.flat_fill(m).is_none() && hc.flat_grain(m) == 0.0 && hc.rim_of(m).is_none()));
            assert!(hc.card_light().is_none() && hc.bar_band().is_none());
            // A theme file keeps its solid tints.
            let file = Theme::from_file_text(GRAPHITE).unwrap().resolve_fx(&sys, true);
            assert!(!file.builtin && file.flat_fill(Material::Panel).is_none() && file.card_light().is_none());
        }
    }

    #[test]
    fn flat_v2s_edge_light_is_one_dip_at_every_scale() {
        use crate::syslook::SysLook;
        for light in [false, true] {
            let sys = SysLook { light, ..Default::default() };
            let flat = Theme::graphite().resolve_fx(&sys, true);
            let mut raw = Theme::graphite();
            raw.finish = Finish::Glass;
            let glass = raw.resolve_fx(&sys, true);
            let file = Theme::from_file_text(GRAPHITE).unwrap().resolve_fx(&sys, true);
            for s in [1.0f32, 1.25, 1.5, 2.0] {
                for m in [Material::Dock, Material::Panel, Material::Menu, Material::Tip] {
                    // Flat: 1 DIP (all clear since 2026-11-12).
                    assert_eq!(flat.rim_width(m, s), 1.0, "{m:?} {s}");
                    assert_eq!(flat.rim_of(m).map(|r| r.map(|w| w.a)), Some([0; 3]), "{m:?}");
                    // A glass and a theme file: one device pixel.
                    assert!((glass.rim_width(m, s) * s - 1.0).abs() < 1e-6, "glass {m:?} {s}");
                    assert!((file.rim_width(m, s) * s - 1.0).abs() < 1e-6, "file {m:?} {s}");
                }
            }
            // The plate a panel draws carries it.
            assert_eq!(flat.plate_coat(Material::Panel, flat.popup_tint(), 1.5).rim.map(|r| r.1), Some(1.0));
            assert_eq!(glass.plate_coat(Material::Panel, glass.popup_tint(), 1.5).rim.map(|r| r.1), Some(1.0 / 1.5));
        }
    }

    #[test]
    fn flat_v2s_card_light_follows_the_raised_things_state() {
        use crate::syslook::SysLook;
        for light in [false, true] {
            let sys = SysLook { light, ..Default::default() };
            let t = Theme::graphite().resolve_fx(&sys, true);
            let (lit, drop) = t.card_light().unwrap();
            // At rest, hovered, disabled, the keyboard's cursor: lit and
            // dropped. Pressed or open: no light (pushed in), the drop
            // stays. On (Selected): neither (the "on" fill stands alone).
            for s in [State::Rest, State::Hover, State::Disabled, State::Cursor] {
                assert_eq!(t.card_fx(s), Some(CardFx { light: lit, drop }), "{s:?}");
            }
            for s in [State::Pressed, State::Open] {
                assert_eq!(t.card_fx(s), Some(CardFx { light: Rgba { a: 0, ..lit }, drop }), "{s:?}");
            }
            assert_eq!(t.card_fx(State::Selected), None);
            // A Flat the PC forces keeps it (only the grain goes).
            let forced = Theme::graphite().resolve_fx(&SysLook { light, transparency: false, ..Default::default() }, true);
            assert_eq!(forced.card_fx(State::Rest), t.card_fx(State::Rest));
            // A glass, high contrast, a theme file: none.
            let mut raw = Theme::graphite();
            raw.finish = Finish::Glass;
            assert!(raw.resolve_fx(&sys, true).card_fx(State::Rest).is_none());
            let hc = Theme::graphite().resolve_fx(&SysLook { light, high_contrast: Some(hc_black()), ..Default::default() }, true);
            assert!(hc.card_fx(State::Rest).is_none());
            assert!(Theme::from_file_text(GRAPHITE).unwrap().resolve_fx(&sys, true).card_fx(State::Rest).is_none());
        }
    }

    #[test]
    fn flat_v2_secondary_text_reads_on_its_plates_cards_and_rows() {
        use crate::syslook::SysLook;
        for light in [false, true] {
            let t = Theme::graphite().resolve_fx(&SysLook { light, ..Default::default() }, true);
            let (sec, ter) = (t.text_secondary(), t.text_tertiary());
            let (rest, hover, pressed) = t.card_states();
            let rows = [State::Hover, State::Pressed, State::Selected].map(|s| t.fill_row(s));
            for m in [Material::Panel, Material::Menu] {
                for p in t.plate_seen(m, Rgba { r: 0x80, g: 0x80, b: 0x80, a: 255 }) {
                    let mut unders = vec![p];
                    unders.extend(rows.iter().map(|&r| over(r, p)));
                    if m == Material::Panel {
                        unders.extend([rest, hover, pressed].map(|c| over(c, p)));
                    }
                    for u in unders {
                        let c = contrast(over(sec, u), u);
                        assert!(c >= 4.5, "light {light} {m:?} secondary {:.2} on {u:?}", c);
                    }
                    assert!(contrast(over(ter, p), p) >= 3.0, "light {light} {m:?} tertiary");
                }
            }
            println!("flat v2 light {light}: secondary {:.3}, tertiary {:.3}", sec.a as f32 / 255.0, ter.a as f32 / 255.0);
            // The dark menus' selected row asks the most: .62 goes to about .66.
            if !light {
                let a = sec.a as f32 / 255.0;
                assert!((0.60..=0.68).contains(&a), "dark secondary {a:.3}");
            } else {
                assert_eq!(sec.a, (TEXT_LIGHT.secondary * 255.0).round() as u8, "light keeps .60");
            }
        }
    }
}
