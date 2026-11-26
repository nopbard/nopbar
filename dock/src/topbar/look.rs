//! What the bar stands on (topbar.style; the Windows top bar,
//! docs/design/harmony/topbar-mac-results.md): its ground, the ink its
//! text and icons take, the halo under that ink and the fill of the cell
//! whose menu or panel is up.
//!
//! Glass chooses its tint and ink from the wallpaper strip.
//! Clear: no plate at all, the text and icons straight on the
//! wallpaper, black over a bright one and white over a dark one, each with
//! a faint halo. Frost: one quiet frosted plate whatever the material
//! recipe is. Recipe: the dock's and menus' material (theme's M0, the
//! glass path in force). Solid: an opaque band. Windows' transparency
//! effects off and high contrast make every style solid (12.1, 12.3);
//! Frost without a blur to stand on is solid too.
//!
//! Clear's ink is chosen without looking at the screen: the wallpaper
//! file of the bar's monitor (IDesktopWallpaper) is decoded small on a
//! helper thread, placed as Windows places it, and the strip the bar
//! covers looked at column by column (`measure`), each column's mean and
//! its darkest and brightest point down the bar: the bar takes the ink of
//! the mean, and each cell the ink its own columns' extremes ask, with a
//! guard plate where the wallpaper under it is too mixed for either
//! (`cell_look`). A plain color desktop is that color. A slideshow, or a
//! picture that cannot be read, cannot be known from a file: the bar is
//! Solid then, and a failed read is tried again a few times.

use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::{icons::Icon, theme::{AdaptiveGlass, Rgba, Theme}};

/// The bar's styles, in config::TOPBAR_STYLES' order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Clear,
    Frost,
    Recipe,
    Solid,
    Glass,
}

impl Style {
    /// Index into config::TOPBAR_STYLES (TopbarCfg::style).
    pub fn of(i: usize) -> Style {
        const STYLES: [Style; crate::config::TOPBAR_STYLES.len()] = [Style::Clear, Style::Frost, Style::Recipe, Style::Solid, Style::Glass];
        STYLES[i.min(STYLES.len() - 1)]
    }
}

/// What the PC allows the bar: Windows' transparency effects, a blur
/// behind it (NopBar's blur switch on, not Low-spec mode) and the device
/// it is drawn on (`software`: WARP, chosen or a recovery's fallback).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Env {
    pub transparency: bool,
    pub blur: bool,
    pub software: bool,
}

impl Default for Env {
    fn default() -> Self {
        Env { transparency: true, blur: true, software: false }
    }
}

impl Env {
    /// As the config, Windows and the device have it now (Windows' switch
    /// read from the registry, as syslook does).
    pub fn now(cfg: &crate::config::Config) -> Env {
        use windows::core::w;
        let personalize = w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
        let transparency = crate::syslook::dword(personalize, w!("EnableTransparency")).is_none_or(|v| v != 0);
        Env::of(cfg, transparency, crate::gfx::software_in_force())
    }

    /// Pure: Env::now from what it reads.
    pub fn of(cfg: &crate::config::Config, transparency: bool, software: bool) -> Env {
        Env { transparency, blur: cfg.effects.blur && !cfg.effects.low_spec, software }
    }

    /// A blur really stands behind the bar: the switch on and a device
    /// that blurs. The rule every other surface falls back to Flat by
    /// (Theme::resolve, theme::glass_blur), whatever finish is chosen: a
    /// Flat finish with the frost bar keeps its frost (the owner's pick);
    /// a blur the device or Low-spec mode takes away makes the frost bar
    /// Solid with every glass (2026-11-11 final review, 1).
    pub fn blurs(self) -> bool {
        crate::theme::glass_blur(self.blur, self.software)
    }
}

/// Pure: the style in force: `style` unless high contrast or transparency
/// off make it solid; Frost without a blur (Env::blurs) is solid too (a
/// frost plate with nothing under it is only a grey band).
pub fn in_force(style: Style, theme: &Theme, env: Env) -> Style {
    if theme.high_contrast || !env.transparency || (style == Style::Frost && !env.blurs()) {
        Style::Solid
    } else {
        style
    }
}

/// Frost's plate (the mock-up's: `#1E1E1E` / `#F6F6F6` at .55 over the
/// blur, the blur's saturation lifted to FROST_SAT), the same whatever
/// the material recipe: the blur through glass v3's graph at saturation
/// alone (glass::refresh_fixed: no Luminosity layer, no top light, no
/// grain), the tint as it is.
pub const FROST_SAT: f32 = 1.0;
pub const FROST_DARK: Rgba = Rgba { r: 0x1E, g: 0x1E, b: 0x1E, a: 240 };
pub const FROST_LIGHT: Rgba = Rgba { r: 0xF6, g: 0xF6, b: 0xF6, a: 240 };
/// Solid's band: material M0 opaque (Flat's `#1E1E1E` / `#F0F0F0`).
pub const SOLID_DARK: Rgba = Rgba { r: 0x1E, g: 0x1E, b: 0x1E, a: 255 };
pub const SOLID_LIGHT: Rgba = Rgba { r: 0xF0, g: 0xF0, b: 0xF0, a: 255 };
/// Clear's ink: black over a bright wallpaper, white over a dark one (the
/// mock-up's .86 / .92).
pub const INK_ON_LIGHT: Rgba = Rgba { r: 0, g: 0, b: 0, a: 219 };
pub const INK_ON_DARK: Rgba = Rgba { r: 255, g: 255, b: 255, a: 235 };
/// The open cell's fill (the Windows shell: white .18 on a dark bar, black .10 on
/// a light one).
pub const MARK_ON_DARK: Rgba = Rgba { r: 255, g: 255, b: 255, a: 46 };
pub const MARK_ON_LIGHT: Rgba = Rgba { r: 0, g: 0, b: 0, a: 26 };
/// A relative luminance over which black text reads better than white:
/// both have the same contrast over this ((1.05) / (L + .05) = (L + .05)
/// / .05).
pub const CROSSOVER: f32 = 0.179;

/// The faint halo under clear's ink (the mock-up's text shadow). The bar
/// lays it as a compositor shadow of each cell's content
/// (TopBar::halo_slots, gfx::halo_shadow): a cell's content is shown in
/// its own ink through a mask and holds glyphs as well as text.
pub use crate::theme::Halo;

/// White ink: a dark shade a pixel down (`0 1px 2px rgba(0,0,0,.45)`).
pub const HALO_UNDER_WHITE: Halo = Halo { color: Rgba { r: 0, g: 0, b: 0, a: 115 }, dy: 1.0, blur: 2.0 };
/// Black ink: a light glow around it, tighter and fainter than the
/// mock-up's `0 0 2px rgba(255,255,255,.6)`: .5, 1.5 DIP (a wider one
/// spread into a pale rim on a mid wallpaper: final/issues.md 1).
pub const HALO_UNDER_BLACK: Halo = Halo { color: Rgba { r: 255, g: 255, b: 255, a: 128 }, dy: 0.0, blur: 1.5 };

/// What the bar is drawn from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ground {
    /// Brightness of the actual bar surface (wallpaper for Clear).
    pub mark_light: bool,
    /// The style in force (in_force).
    pub style: Style,
    /// The plate's tint (alpha 0: no plate).
    pub tint: Rgba,
    /// The blur shows under the tint.
    pub backdrop: bool,
    /// The backdrop is a mica strip; otherwise it is the compositor's host brush.
    pub mica: bool,
    /// A line along the bar's inner edge: (color, width). Glass uses
    /// physical pixels and the other styles use DIP.
    pub edge: Option<(Rgba, f32)>,
    /// The text's and icons' color.
    pub ink: Rgba,
    /// Under the ink (clear only).
    pub halo: Option<Halo>,
    /// The fill of a cell that is pressed or whose menu or panel is up
    /// (None: high contrast's looks, capsule::look).
    pub mark: Option<Rgba>,
    /// An opaque plate's fall from top to bottom (Flat v2's M0: the solid
    /// band, the recipe on Flat; None: the tint as one color).
    pub fall: Option<(Rgba, Rgba)>,
    /// The grain over the plate (Flat v2's, the Flat one chose; 0: none).
    pub grain: f32,
    /// How much denser the see-through plate (frost, the recipe over the
    /// blur) is made over the wallpaper under the bar, 0..1: another coat
    /// of the tint's own grey (glass::fold), only as much as the bar's ink
    /// needs to keep its contrast there (deepen_for). 0: as it is.
    pub deepen: f32,
}

impl Ground {
    /// The tint as laid: `tint` (the one the brush asks: glass::tint_for)
    /// with `deepen` folded in.
    pub fn laid(&self, tint: Rgba) -> Rgba {
        if self.deepen <= 0.0 {
            return tint;
        }
        crate::glass::fold(tint, Rgba { a: (self.deepen.clamp(0.0, 1.0) * 255.0).round() as u8, ..tint })
    }
}

/// The steps deepen_for raises a plate's density by.
pub const DEEPEN_STEPS: u32 = 40;

/// Pure: what the bar's see-through plate in `style` (frost: its tint
/// over the blur, saturation alone so a grey stays grey; the recipe: the
/// glass's M0 as text sees it, Theme::plate_seen: every fallback level,
/// the top light and the grain at their bounds) shows over a wallpaper of
/// luminance `l`, made denser by `deepen`. Empty: no see-through plate.
pub fn plates_over(theme: &Theme, style: Style, tint: Rgba, l: f32, deepen: f32) -> Vec<Rgba> {
    let g = grey(l);
    let wp = Rgba { r: g, g, b: g, a: 255 };
    match style {
        Style::Frost => {
            let t = crate::glass::fold(tint, Rgba { a: (deepen.clamp(0.0, 1.0) * 255.0).round() as u8, ..tint });
            vec![crate::theme::over(t, wp)]
        }
        Style::Recipe => theme.plate_seen_deeper(crate::theme::Material::Bar, wp, deepen),
        _ => Vec::new(),
    }
}

/// Pure: the contrast of the bar's `ink` over `plate` as it is, no halo
/// counted (deepen_for).
pub fn bar_contrast(ink: Rgba, plate: Rgba) -> f32 {
    crate::theme::contrast(crate::theme::over(ink, plate), plate)
}

/// Pure: how much denser (Ground::deepen) a frost or recipe plate over the
/// blur must be for `ink` to keep MIN_CONTRAST over the plate itself
/// (bar_contrast), over every wallpaper luminance in `lums` (the extremes
/// under the bar). 0 when it keeps it as it is, or the plate is not
/// see-through (2026-11-11 final audit 2: a dark frost bar over a pale
/// wallpaper kept white text at under 4:1; a wallpaper half dark, half
/// bright gets one plate for both). The bar runs the screen's whole width
/// and is read first, so its ink keeps body text's 4.5:1 on every finish.
pub fn deepen_for(theme: &Theme, style: Style, tint: Rgba, ink: Rgba, lums: &[f32]) -> f32 {
    if !matches!(style, Style::Frost | Style::Recipe) || lums.is_empty() {
        return 0.0;
    }
    let reads = |d: f32| lums.iter().all(|&l| plates_over(theme, style, tint, l, d).into_iter().all(|p| bar_contrast(ink, p) >= MIN_CONTRAST));
    (0..=DEEPEN_STEPS).map(|k| k as f32 / DEEPEN_STEPS as f32).find(|&d| reads(d)).unwrap_or(1.0)
}

/// What a see-through plate is made dense for while the wallpaper under
/// the bar is not known (Wall::Pending, Wall::Unknown: not measured yet,
/// a read that failed, a slideshow): black and white both, the worst
/// either ink can stand on. The plate then keeps its ink readable over
/// anything, and lightens to what the wallpaper asks once it is read
/// (2026-11-11 final review 1: an unknown wallpaper kept the plate as it
/// is, the dark frost's text under 4.5:1 over a bright slideshow).
pub const UNKNOWN_LUMS: [f32; 2] = [0.0, 1.0];

/// Pure: the bar's see-through plate wants the wallpaper under it
/// (deepen_for): frost, and the recipe over the blur.
pub fn reads_wall(style: Style) -> bool {
    matches!(style, Style::Clear | Style::Frost | Style::Recipe | Style::Glass)
}

/// Pure: whether the bar reads as light (black ink) over a wallpaper of
/// mean relative luminance `wall` (None: not known: the theme's mode).
pub fn light_over(wall: Option<f32>, theme_light: bool) -> bool {
    wall.map(|l| l > CROSSOVER).unwrap_or(theme_light)
}

/// Pure: the bar's ground in `theme` for `style` in `env`, the wallpaper
/// under it `wall`. Clear over a wallpaper that cannot be
/// known (a slideshow, a picture that cannot be read) is Solid: an
/// opaque band keeps the text readable whatever is shown (2026-11-11
/// review). Its cells may each differ from the bar's ink (cell_look).
/// Frost's and the recipe's see-through plate over a wallpaper not known
/// is as dense as over the worst one (UNKNOWN_LUMS).
pub fn ground(theme: &Theme, style: Style, env: Env, wall: &Wall) -> Ground {
    let style = match in_force(style, theme, env) {
        Style::Clear if matches!(wall, Wall::Unknown { .. }) => Style::Solid,
        Style::Glass if !matches!(wall, Wall::Glass(_) | Wall::Live | Wall::Pending) => Style::Solid,
        s => s,
    };
    // The darkest and brightest of the wallpaper under the bar (a see-
    // through plate's density: deepen_for); not known (not measured yet,
    // a read that failed, a slideshow): the worst of both (UNKNOWN_LUMS).
    let lums = match wall {
        Wall::Known(cols) if !cols.is_empty() => extremes(cols),
        _ => UNKNOWN_LUMS.to_vec(),
    };
    let edge = theme.outline_of(crate::theme::Role::Raised, crate::theme::State::Rest)
        .or_else(|| (style == Style::Frost).then_some((Rgba { r: 128, g: 128, b: 128, a: 32 }, 1.0)));
    let mark = |light: bool| (!theme.high_contrast).then_some(if light { MARK_ON_LIGHT } else { MARK_ON_DARK });
    let ink = theme.topbar_text();
    match style {
        Style::Clear => {
            let light = light_over(wall.mean(), theme.is_light);
            Ground {
                mark_light: light,
                style,
                tint: Rgba { a: 0, ..SOLID_DARK },
                backdrop: false,
                mica: false,
                edge,
                ink: if light { INK_ON_LIGHT } else { INK_ON_DARK },
                halo: Some(if light { HALO_UNDER_BLACK } else { HALO_UNDER_WHITE }),
                mark: mark(light),
                fall: None,
                grain: 0.0,
                deepen: 0.0,
            }
        }
        Style::Frost => {
            let tint = if theme.is_light { FROST_LIGHT } else { FROST_DARK };
            let deepen = deepen_for(theme, style, tint, ink, &lums);
            Ground { mark_light: crate::theme::luminance(tint) > CROSSOVER, style, tint, backdrop: true, mica: false, edge, ink, halo: None, mark: mark(theme.is_light), fall: None, grain: 0.0, deepen }
        }
        Style::Glass => {
            let glass = match wall {
                Wall::Glass(glass) => *glass,
                // Live and pending use the midpoint of the system tint range.
                _ => AdaptiveGlass::new(if theme.is_light { 0.55 } else { 0.15 }, None),
            };
            let (ink, line) = if glass.light {
                (Rgba { r: 17, g: 17, b: 20, a: 255 }, Rgba { r: 0, g: 0, b: 0, a: 15 })
            } else {
                (Rgba { r: 255, g: 255, b: 255, a: 255 }, Rgba { r: 255, g: 255, b: 255, a: 15 })
            };
            Ground { mark_light: glass.light, style, tint: glass.tint, backdrop: true, mica: matches!(wall, Wall::Glass(_)), edge: Some((line, 1.0)), ink, halo: None, mark: mark(glass.light), fall: None, grain: 0.0, deepen: 0.0 }
        }
        Style::Recipe => {
            // The glass in force (material M0: the tint over the blur
            // through the glass's graph, glass v3's tokens and fallback
            // levels; opaque on Flat). TopBar::apply_theme lays the tint
            // the brush it really has asks (glass::refresh_backdrop).
            let g = super::bar_ground(theme);
            let flat = !g.backdrop;
            let fall = theme.flat_fill(crate::theme::Material::Bar).filter(|_| flat);
            let grain = if flat { theme.flat_grain(crate::theme::Material::Bar) } else { 0.0 };
            let deepen = if g.backdrop { deepen_for(theme, style, g.tint, ink, &lums) } else { 0.0 };
            Ground { mark_light: crate::theme::luminance(g.tint) > CROSSOVER, style, tint: g.tint, backdrop: g.backdrop, mica: false, edge: g.edge, ink, halo: None, mark: mark(theme.is_light), fall, grain, deepen }
        }
        Style::Solid => {
            // High contrast: its Window color (the resolved theme's M0).
            // The built-in theme: Flat v2's M0 whatever the finish (its
            // fall, its mean as the tint), the grain on the Flat one chose.
            let fall = theme.bar_band();
            let tint = if theme.high_contrast {
                Rgba { a: 255, ..theme.material(crate::theme::Material::Bar) }
            } else if let Some((top, bottom)) = fall {
                let v = (top.r as u16 + bottom.r as u16).div_ceil(2) as u8;
                Rgba { r: v, g: v, b: v, a: 255 }
            } else if theme.is_light {
                SOLID_LIGHT
            } else {
                SOLID_DARK
            };
            Ground { mark_light: crate::theme::luminance(tint) > CROSSOVER, style, tint, backdrop: false, mica: false, edge, ink, halo: None, mark: mark(theme.is_light), fall, grain: theme.flat_grain(crate::theme::Material::Bar), deepen: 0.0 }
        }
    }
}

// ---------------------------------------------------------------- the wallpaper

/// What is known of the wallpaper under the bar (clear's ink).
#[derive(Clone, Debug, PartialEq)]
pub enum Wall {
    /// Not measured yet: the theme's mode meanwhile.
    Pending,
    /// The bar's own rectangle of it, column by column from the bar's
    /// left edge to its right (one column: a plain color).
    Known(Vec<Col>),
    /// The mica picture is available and the strip's glass has been chosen.
    Glass(AdaptiveGlass),
    /// A live wallpaper is drawn by DWM's host backdrop.
    Live,
    /// Its file says nothing of what is shown: a slideshow, or a picture
    /// that could not be read or placed. The bar then stands on a plate
    /// that keeps its text readable over anything (Solid). `retry`: a
    /// read that failed, tried again a few times (RETRY_MS).
    Unknown { retry: bool },
}

/// One column of the wallpaper under the bar, down the bar's height:
/// its mean relative luminance (the bar's ink) and its darkest and
/// brightest point (a cell's: a band across the bar, a dark row under
/// bright ones, is under the text whatever the mean says; 2026-11-11
/// confirm review).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Col {
    pub mean: f32,
    pub lo: f32,
    pub hi: f32,
}

impl Col {
    /// A column all of one luminance (a plain color).
    pub fn flat(l: f32) -> Col {
        Col { mean: l, lo: l, hi: l }
    }
}

/// Pure: the darkest and the brightest point under `cols` (empty: none,
/// cell_look then gives None).
pub fn extremes(cols: &[Col]) -> Vec<f32> {
    if cols.is_empty() {
        return Vec::new();
    }
    let lo = cols.iter().map(|c| c.lo).fold(f32::INFINITY, f32::min);
    let hi = cols.iter().map(|c| c.hi).fold(f32::NEG_INFINITY, f32::max);
    vec![lo, hi]
}

impl Wall {
    /// The mean relative luminance across the bar (None: not known).
    pub fn mean(&self) -> Option<f32> {
        match self {
            Wall::Known(cols) if !cols.is_empty() => Some(cols.iter().map(|c| c.mean).sum::<f32>() / cols.len() as f32),
            _ => None,
        }
    }
}

/// A failed read is tried again after these (ms), then left until the
/// wallpaper, the displays or the bar change.
pub const RETRY_MS: [u32; 3] = [2_000, 10_000, 60_000];

/// Pure: the delay before try `tries` + 1 of a read that failed (None:
/// no more).
pub fn retry_after(wall: &Wall, tries: usize) -> Option<u32> {
    match wall {
        Wall::Unknown { retry: true } => RETRY_MS.get(tries).copied(),
        _ => None,
    }
}

/// The least contrast clear's text keeps with what is right under it
/// (WCAG's for text, 4.5:1), the halo not counted.
pub const MIN_CONTRAST: f32 = 4.5;

/// One cell's own look on a clear bar (`cell_look`): its ink, the halo
/// and the open fill that go with it, and the guard plate under it when
/// the wallpaper there is too mixed for either ink alone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellLook {
    pub ink: Rgba,
    pub halo: Halo,
    pub mark: Rgba,
    /// A plate the cell's size (the open fill's shape) in the color
    /// opposite the ink, as see-through as the contrast allows.
    pub guard: Option<Rgba>,
}

/// Pure: an sRGB grey (0..255) of relative luminance `l`.
pub fn grey(l: f32) -> u8 {
    let l = l.clamp(0.0, 1.0);
    let v = if l <= 0.0031308 { l * 12.92 } else { 1.055 * l.powf(1.0 / 2.4) - 0.055 };
    (v * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Pure: the contrast `ink` keeps over a wallpaper of luminance `l` with
/// `plate` (if any) between them, both blended as the compositor blends
/// (theme::over, in sRGB).
pub fn ink_contrast(ink: Rgba, plate: Option<Rgba>, l: f32) -> f32 {
    let g = grey(l);
    let mut bg = Rgba { r: g, g, b: g, a: 255 };
    if let Some(p) = plate {
        bg = crate::theme::over(p, bg);
    }
    crate::theme::contrast(crate::theme::over(ink, bg), bg)
}

fn ink_of(light: bool) -> Rgba {
    if light {
        INK_ON_LIGHT
    } else {
        INK_ON_DARK
    }
}

/// Pure: the color that shows content drawn in `drawn` as `target`
/// through a mask of the content's own alpha (TopBar::ink_slot): its
/// alpha divided by the drawing's (at most 1).
pub fn mask_ink(target: Rgba, drawn: Rgba) -> Rgba {
    let a = if drawn.a == 0 { 255.0 } else { (target.a as f32 * 255.0 / drawn.a as f32).min(255.0) };
    Rgba { a: a.round() as u8, ..target }
}

/// Pure: what `target` shows as on content drawn in `drawn` (mask_ink):
/// no stronger than the drawing was.
pub fn shown_ink(target: Rgba, drawn: Rgba) -> Rgba {
    Rgba { a: target.a.min(drawn.a), ..target }
}

/// Pure: a clear bar's cell over wallpaper luminances `cols` (what is
/// under it: its columns' extremes, `extremes`; empty: None), its content
/// drawn in `drawn` (the
/// bar's ink: another ink shows through a mask, `shown_ink`). The ink is
/// the one whose worst
/// contrast over the cell's darkest and brightest columns is better
/// (`fixed`: this one, for content drawn in the bar's own ink that cannot
/// be shown in another: the battery, our tray icon). If even that one
/// falls under MIN_CONTRAST somewhere (white and black under one cell,
/// or a mid grey), a guard plate in the opposite color goes under it, as
/// see-through as still keeps MIN_CONTRAST over both (2026-11-11 review:
/// one mean for the whole bar put black text over the black part of a
/// mostly white strip).
pub fn cell_look(cols: &[f32], fixed: Option<bool>, drawn: Rgba) -> Option<CellLook> {
    let lo = cols.iter().copied().fold(f32::INFINITY, f32::min);
    let hi = cols.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    if !lo.is_finite() || !hi.is_finite() {
        return None;
    }
    let ink = |light: bool| shown_ink(ink_of(light), drawn);
    let worst = |light: bool, plate: Option<Rgba>| ink_contrast(ink(light), plate, lo).min(ink_contrast(ink(light), plate, hi));
    let light = fixed.unwrap_or_else(|| worst(true, None) >= worst(false, None));
    let guard = (worst(light, None) < MIN_CONTRAST).then(|| {
        let base = if light { SOLID_LIGHT } else { SOLID_DARK };
        (1..=20).map(|k| Rgba { a: (k as f32 * 255.0 / 20.0).round() as u8, ..base }).find(|p| worst(light, Some(*p)) >= MIN_CONTRAST).unwrap_or(base)
    });
    Some(CellLook {
        ink: ink(light),
        halo: if light { HALO_UNDER_BLACK } else { HALO_UNDER_WHITE },
        mark: if light { MARK_ON_LIGHT } else { MARK_ON_DARK },
        guard,
    })
}

/// Pure: the columns of `cols` (across a bar `width` DIP wide) under the
/// span `x`..`x + w` DIP of it (at least one).
pub fn cols_under<T>(cols: &[T], width: f32, x: f32, w: f32) -> &[T] {
    let n = cols.len();
    if n == 0 || width <= 0.0 {
        return &[];
    }
    let at = |v: f32| ((v / width).clamp(0.0, 1.0) * n as f32) as usize;
    let a = at(x).min(n - 1);
    let b = (((x + w) / width).clamp(0.0, 1.0) * n as f32).ceil() as usize;
    &cols[a..b.clamp(a + 1, n)]
}

/// What the cells' looks are read from (TopBar::looks_in): the wallpaper
/// measured (its version: the bar counts each one it takes), the ground's
/// style and ink, the bar's width and each cell's span (x, w DIP) with
/// the ink it is fixed to (cell_look; None: a cell that takes no look of
/// its own: no width, another app's tray icon). What is lit (hover,
/// press, open) is not in it.
#[derive(Clone, Debug, PartialEq)]
pub struct LooksIn {
    pub wall: u64,
    pub style: Style,
    pub ink: Rgba,
    pub width: f32,
    pub cells: Vec<Option<(f32, f32, Option<bool>)>>,
}

/// Pure: each cell's look on `wall` for `k` (cell_look over the extremes
/// of the columns under it): None off clear, while the wallpaper is not
/// known, and for cells that take none.
pub fn looks_of(wall: &Wall, k: &LooksIn) -> Vec<Option<CellLook>> {
    let Wall::Known(cols) = wall else { return vec![None; k.cells.len()] };
    if k.style != Style::Clear {
        return vec![None; k.cells.len()];
    }
    k.cells.iter().map(|c| c.and_then(|(x, w, fixed)| cell_look(&extremes(cols_under(cols, k.width, x, w)), fixed, k.ink))).collect()
}

/// The cells' looks, read again only when what they are read from
/// changes (2026-11-11 confirm review: a hover moving between cells, or
/// the clock ticking, read every cell's columns again).
#[derive(Default)]
pub struct CellLooks {
    read_for: Option<LooksIn>,
    pub looks: Vec<Option<CellLook>>,
    /// How many times they were read.
    pub reads: usize,
}

impl CellLooks {
    /// Reads them for `k` on `wall` unless that is what they were read
    /// for; true if they changed.
    pub fn refresh(&mut self, wall: &Wall, k: LooksIn) -> bool {
        if self.read_for.as_ref() == Some(&k) {
            return false;
        }
        let looks = looks_of(wall, &k);
        self.reads += 1;
        self.read_for = Some(k);
        if looks == self.looks {
            return false;
        }
        self.looks = looks;
        true
    }

    /// Cell `i`'s look (None: the ground's).
    pub fn get(&self, i: usize) -> Option<CellLook> {
        self.looks.get(i).copied().flatten()
    }
}

/// A guard plate: where it is (capsule::Spot) and its color.
pub type Plate = (super::capsule::Spot, Rgba);

/// What has to change on a guard plate going from `was` to `now` (None:
/// hidden): only what differs is set, and its color brush is kept and
/// recolored (TopBar's Guards).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlateEdit {
    pub place: bool,
    pub color: bool,
    /// Some(true): show it; Some(false): hide it.
    pub show: Option<bool>,
}

/// Pure: PlateEdit from `was` to `now`.
pub fn plate_edit(was: Option<Plate>, now: Option<Plate>) -> PlateEdit {
    match (was, now) {
        (_, None) => PlateEdit { show: was.is_some().then_some(false), ..Default::default() },
        (None, Some(_)) => PlateEdit { place: true, color: true, show: Some(true) },
        (Some((ws, wc)), Some((ns, nc))) => PlateEdit { place: ws != ns, color: wc != nc, show: None },
    }
}

/// A helper measured the wallpaper under a bar: lParam = the request's
/// token; the answer waits in `taken(token)`.
pub const WM_WALL: u32 = WM_APP + 44;

/// What a measurement is for: the bar's monitor and the bar's own
/// rectangle on it (px, screen: a taskbar the bar steps around is not
/// under it), the box around every display (a spanned picture is cut
/// from it: another display changing changes this one's part), and the
/// wallpaper's generation (wallpaper_changed). The same key is never
/// measured twice in a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WallKey {
    pub mon: (i32, i32, i32, i32),
    pub bar: (i32, i32, i32, i32),
    pub virt: (i32, i32, i32, i32),
    pub gen: u64,
    pub mica_gen: Option<u64>,
}

pub enum WallRead {
    Standard(Wall),
    Mica { luminance: f32, strip: Icon },
}

static GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static TOKEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
static ANSWERS: std::sync::Mutex<Vec<(u64, WallRead)>> = std::sync::Mutex::new(Vec::new());

/// The wallpaper changed (SPI_SETDESKWALLPAPER) or the displays did
/// (WM_DISPLAYCHANGE): every measurement is stale.
pub fn wallpaper_changed() {
    GEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

fn quad(r: RECT) -> (i32, i32, i32, i32) {
    (r.left, r.top, r.right, r.bottom)
}

fn rect((left, top, right, bottom): (i32, i32, i32, i32)) -> RECT {
    RECT { left, top, right, bottom }
}

pub fn key(mon: RECT, bar: RECT, virt: RECT, mica: bool) -> WallKey {
    WallKey {
        mon: quad(mon), bar: quad(bar), virt: quad(virt),
        gen: if mica { 0 } else { GEN.load(std::sync::atomic::Ordering::SeqCst) },
        mica_gen: mica.then(crate::wallpaper::generation),
    }
}

/// The answer to `token`, once (None: not in, or another bar's).
pub fn taken(token: u64) -> Option<WallRead> {
    let mut a = ANSWERS.lock().unwrap_or_else(|e| e.into_inner());
    let i = a.iter().position(|(t, _)| *t == token)?;
    Some(a.swap_remove(i).1)
}

/// Measures the wallpaper for `k` on a helper; WM_WALL with the returned
/// token tells `hwnd` its answer is in (`taken`).
pub fn ask(hwnd: HWND, k: WallKey) -> u64 {
    let token = TOKEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let h = hwnd.0 as isize;
    let spawned = std::thread::Builder::new().name("bar-wall".into()).spawn(move || {
        use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
        let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
        let (mon, bar) = (rect(k.mon), rect(k.bar));
        let got = if k.mica_gen.is_some() {
            read_glass(crate::wallpaper::live_wallpaper(), || {
                crate::wallpaper::mica_picture(mon).and_then(|img| mica_strip(&img, mon, bar))
            })
        } else {
            WallRead::Standard(unsafe { measure(mon, bar, rect(k.virt)) })
        };
        if com {
            unsafe { CoUninitialize() };
        }
        let mut a = ANSWERS.lock().unwrap_or_else(|e| e.into_inner());
        // (Answers no bar took: a bar gone, or one asked again.)
        if a.len() > 16 {
            a.remove(0);
        }
        a.push((token, got));
        drop(a);
        unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), WM_WALL, WPARAM(0), LPARAM(token as isize));
        }
    });
    if spawned.is_err() {
        crate::log!("topbar: could not start the wallpaper helper");
    }
    token
}

fn read_glass(live: bool, read_mica: impl FnOnce() -> Option<(f32, Icon)>) -> WallRead {
    if live {
        WallRead::Standard(Wall::Live)
    } else {
        read_mica().map_or(WallRead::Standard(Wall::Unknown { retry: true }), |(luminance, strip)| WallRead::Mica { luminance, strip })
    }
}

#[test]
fn live_glass_uses_the_host_and_system_theme_without_reading_a_file() {
    let WallRead::Standard(wall) = read_glass(true, || panic!("live wallpaper must skip mica")) else {
        panic!("live wallpaper must use the host");
    };
    assert_eq!(wall, Wall::Live);
    assert_eq!(retry_after(&wall, 0), None);
    for is_light in [false, true] {
        let theme = Theme { is_light, ..Theme::graphite() };
        let look = ground(&theme, Style::Glass, Env::default(), &wall);
        assert!(look.backdrop && !look.mica && look.style == Style::Glass);
        let static_wall = Wall::Glass(AdaptiveGlass::new(if is_light { 0.55 } else { 0.15 }, None));
        assert_ne!(look, ground(&theme, Style::Glass, Env::default(), &static_wall));
        assert_eq!(look.mark_light, is_light);
    }
}

/// Pure: where the bar at `bar` (px, screen) is on a picture `pw` x `ph`
/// standing for the monitor at `mon`: (x0, y0, x1, y1) picture px, at
/// least one each way (None: the bar is not on that monitor).
pub fn bar_crop(mon: RECT, bar: RECT, pw: u32, ph: u32) -> Option<(u32, u32, u32, u32)> {
    let (mw, mh) = ((mon.right - mon.left) as f64, (mon.bottom - mon.top) as f64);
    if mw <= 0.0 || mh <= 0.0 || pw == 0 || ph == 0 {
        return None;
    }
    let (l, t) = (bar.left.max(mon.left), bar.top.max(mon.top));
    let (r, b) = (bar.right.min(mon.right), bar.bottom.min(mon.bottom));
    if r <= l || b <= t {
        return None;
    }
    let (kx, ky) = (pw as f64 / mw, ph as f64 / mh);
    let x0 = (((l - mon.left) as f64 * kx).floor() as u32).min(pw - 1);
    let y0 = (((t - mon.top) as f64 * ky).floor() as u32).min(ph - 1);
    let x1 = (((r - mon.left) as f64 * kx).ceil() as u32).clamp(x0 + 1, pw);
    let y1 = (((b - mon.top) as f64 * ky).ceil() as u32).clamp(y0 + 1, ph);
    Some((x0, y0, x1, y1))
}

/// The mean relative luminance and the saturated mica pixels under this bar.
pub fn mica_strip(picture: &crate::wallpaper::MicaPicture, mon: RECT, bar: RECT) -> Option<(f32, Icon)> {
    let img = &picture.blurred;
    let crop = bar_crop(mon, bar, img.width, img.height)?;
    let (x0, y0, x1, y1) = crop;
    let cols = columns(&picture.source.pixels, img.width, img.height, crop);
    let luminance = Wall::Known(cols).mean()?;
    let strip = Icon {
        width: x1 - x0,
        height: y1 - y0,
        pixels: (y0..y1).flat_map(|y| {
            let at = (y * img.width * 4) as usize;
            img.pixels[at + x0 as usize * 4..at + x1 as usize * 4].iter().copied()
        }).collect(),
    };
    Some((luminance, crate::desklayer::backdrop::saturated(&strip, AdaptiveGlass::SATURATION)))
}

/// The middle part of the bar's height the content and its halo stand
/// on (a cell's darkest and brightest point is looked for there): 3/4,
/// 19.5 of the 26 DIP bar, where the 16 DIP icons and their halo are. A
/// line along the bar's very edge is under no text.
pub const INK_BAND: f32 = 0.75;

/// Pure: rows `y0..y1`'s middle INK_BAND (at least one row).
pub fn ink_rows(y0: u32, y1: u32) -> (u32, u32) {
    let skip = ((y1.saturating_sub(y0)) as f32 * (1.0 - INK_BAND) / 2.0).floor() as u32;
    let (a, b) = (y0 + skip, y1.saturating_sub(skip));
    if a < b {
        (a, b)
    } else {
        (y0, y1)
    }
}

/// Pure: the columns `x0..x1` of an opaque BGRA picture `w` x `h` over
/// the bar's rows `y0..y1`: each one's mean relative luminance down them
/// (averaged in linear light) and its darkest and brightest pixel in their
/// middle (ink_rows; Col). Empty when the box is not in the picture.
pub fn columns(px: &[u8], w: u32, h: u32, (x0, y0, x1, y1): (u32, u32, u32, u32)) -> Vec<Col> {
    if px.len() < (w * h * 4) as usize || x1 > w || y1 > h || x0 >= x1 || y0 >= y1 {
        return Vec::new();
    }
    let ink = ink_rows(y0, y1);
    (x0..x1)
        .map(|x| {
            let (mut sum, mut lo, mut hi) = (0.0f32, f32::INFINITY, f32::NEG_INFINITY);
            for y in y0..y1 {
                let i = ((y * w + x) * 4) as usize;
                let l = crate::theme::luminance(Rgba { b: px[i], g: px[i + 1], r: px[i + 2], a: 255 });
                sum += l;
                if (ink.0..ink.1).contains(&y) {
                    lo = lo.min(l);
                    hi = hi.max(l);
                }
            }
            Col { mean: sum / (y1 - y0) as f32, lo, hi }
        })
        .collect()
}

/// Whether Windows is showing a slideshow (IDesktopWallpaper::GetStatus).
unsafe fn slideshow() -> bool {
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
    use windows::Win32::UI::Shell::{DesktopWallpaper, IDesktopWallpaper, DSS_ENABLED, DSS_SLIDESHOW};
    let Ok(dw) = CoCreateInstance::<_, IDesktopWallpaper>(&DesktopWallpaper, None, CLSCTX_ALL) else { return false };
    dw.GetStatus().is_ok_and(|s| s.0 & DSS_SLIDESHOW.0 != 0 && s.0 & DSS_ENABLED.0 != 0)
}

/// Pure: what the bar knows from what was read: a slideshow's file is
/// not the picture shown (Unknown, not tried again: its next picture
/// comes with no word to us), a plain color is one column, a picture its
/// columns under the bar, and a picture that could not be read is
/// Unknown and tried again.
pub fn judged(slides: bool, desk: Result<Desktop, ()>, mon: RECT, bar: RECT) -> Wall {
    if slides {
        return Wall::Unknown { retry: false };
    }
    match desk {
        Ok(Desktop::Plain([r, g, b])) => Wall::Known(vec![Col::flat(crate::theme::luminance(Rgba { r, g, b, a: 255 }))]),
        Ok(Desktop::Picture(i)) => match bar_crop(mon, bar, i.width, i.height).map(|c| columns(&i.pixels, i.width, i.height, c)) {
            Some(cols) if !cols.is_empty() => Wall::Known(cols),
            _ => Wall::Unknown { retry: true },
        },
        Ok(Desktop::Unreadable) | Err(()) => Wall::Unknown { retry: true },
    }
}

use super::about::Desktop;

#[cfg(test)]
#[test]
fn glass_reads_each_bars_own_strip_and_keeps_an_accessible_fallback() {
    let mon = RECT { left: -96, top: 0, right: 0, bottom: 64 };
    let source = Icon { width: 12, height: 8, pixels: [[20, 20, 20, 255].repeat(48), [240, 240, 240, 255].repeat(48)].concat() };
    let blurred = Icon { width: 12, height: 8, pixels: [90, 90, 90, 255].repeat(96) };
    let mut picture = crate::wallpaper::prepare_mica(source, 0);
    picture.blurred = blurred;
    let theme = Theme::graphite();
    let env = Env::default();
    for (bar, light) in [(RECT { bottom: 16, ..mon }, false), (RECT { top: 48, ..mon }, true)] {
        let (l, strip) = mica_strip(&picture, mon, bar).unwrap();
        assert_eq!((strip.width, strip.height), (12, 2));
        assert_eq!(strip.pixels[0], 90);
        let wall = Wall::Glass(AdaptiveGlass::new(l, None));
        let g = ground(&theme, Style::Glass, env, &wall);
        assert_eq!((g.style, g.backdrop, g.mark_light), (Style::Glass, true, light));
        assert_eq!(g.ink.r, if light { 17 } else { 255 });
        assert_eq!(g.edge.map(|(c, w)| (c.r, c.a, w)), Some((if light { 0 } else { 255 }, 15, 1.0)));
        assert_eq!(ground(&theme, Style::Glass, Env { transparency: false, ..env }, &wall).style, Style::Solid);
        let hc = Theme { high_contrast: true, ..theme.clone() };
        assert_eq!(ground(&hc, Style::Glass, env, &wall).style, Style::Solid);
    }
    let pending = ground(&theme, Style::Glass, env, &Wall::Pending);
    assert_eq!(pending, ground(&theme, Style::Glass, env, &Wall::Live));
    assert_eq!(ground(&theme, Style::Glass, env, &Wall::Unknown { retry: true }).style, Style::Solid);
}

/// The wallpaper under the bar at `bar` on the monitor at `mon`, from its
/// file (never the screen); `virt`, the box around every display. Needs
/// COM.
unsafe fn measure(mon: RECT, bar: RECT, virt: RECT) -> Wall {
    if slideshow() {
        return judged(true, Err(()), mon, bar);
    }
    judged(false, Ok(super::about::desktop_picture(mon, virt)), mon, bar)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syslook::{HighContrast, SysLook};
    use crate::theme::Finish;

    fn themes(light: bool) -> [Theme; 2] {
        let sys = SysLook { light, ..Default::default() };
        let flat = Theme::graphite().resolve_fx(&sys, true);
        let mut glass = Theme::graphite();
        glass.finish = Finish::Glass;
        [flat, glass.resolve_fx(&sys, true)]
    }

    /// The clear style's halo (the mock-up's text shadow, laid under each
    /// cell by the compositor) is the clear style's alone, on every
    /// finish; Frost's plate and saturation are its own whatever the
    /// finish.
    #[test]
    fn the_halo_is_the_clear_styles_and_frost_is_its_own() {
        assert_eq!((HALO_UNDER_WHITE.color.a, HALO_UNDER_WHITE.dy, HALO_UNDER_WHITE.blur), (115, 1.0, 2.0));
        assert_eq!((HALO_UNDER_BLACK.color.r, HALO_UNDER_BLACK.color.a, HALO_UNDER_BLACK.dy, HALO_UNDER_BLACK.blur), (255, 128, 0.0, 1.5));
        let env = Env::default();
        for light in [false, true] {
            let sys = SysLook { light, ..Default::default() };
            let mut frost = None;
            for f in [Finish::Flat, Finish::Glass] {
                let mut t = Theme::graphite();
                t.finish = f;
                let t = t.resolve_fx(&sys, true);
                for style in [Style::Clear, Style::Frost, Style::Recipe, Style::Solid] {
                    for wall in [Wall::Pending, Wall::Known(vec![Col::flat(0.9)]), Wall::Known(vec![Col::flat(0.02)])] {
                        let g = ground(&t, style, env, &wall);
                        assert_eq!(g.halo.is_some(), style == Style::Clear, "{f:?} {style:?}");
                    }
                }
                let g = ground(&t, Style::Frost, env, &Wall::Pending);
                assert!(g.backdrop && g.halo.is_none());
                assert_eq!(*frost.get_or_insert(g.tint), g.tint, "{f:?}: frost is the same plate on every finish");
            }
        }
        const { assert!(FROST_SAT == 1.0, "neutral wallpaper saturation") };
    }

    /// Final audit 2 (2026-11-11): a frost or recipe bar over a wallpaper
    /// half dark and half bright, or a pale one under the dark frost, kept
    /// its ink under 4.5:1 there. Its plate is made denser over what the
    /// wallpaper under it asks (deepen_for, laid by Ground::laid), only as
    /// far as that: the ink keeps MIN_CONTRAST over the plate itself
    /// (final sheet 1), over the darkest and the brightest under it, on
    /// every finish, light and dark.
    #[test]
    fn a_see_through_bar_keeps_its_ink_over_any_wallpaper() {
        let env = Env::default();
        let walls: Vec<Vec<Col>> = vec![
            vec![Col::flat(0.0)],
            vec![Col::flat(1.0)],
            vec![Col::flat(0.7)],
            vec![Col::flat(0.2)],
            (0..20).map(|k| Col::flat(if k < 14 { 1.0 } else { 0.0 })).collect(),
            vec![Col { mean: 0.5, lo: 0.0, hi: 1.0 }],
        ];
        let mut deepened = false;
        for light in [false, true] {
            let sys = SysLook { light, ..Default::default() };
            for f in [Finish::Flat, Finish::Glass] {
                let mut t = Theme::graphite();
                t.finish = f;
                let t = t.resolve_fx(&sys, true);
                for style in [Style::Frost, Style::Recipe] {
                    // Known walls against their own extremes; walls not
                    // known (final review 1: not measured yet, a read
                    // tried again, a slideshow) against black and white.
                    let cases = walls
                        .iter()
                        .map(|c| (Wall::Known(c.clone()), extremes(c)))
                        .chain([Wall::Pending, Wall::Unknown { retry: true }, Wall::Unknown { retry: false }, Wall::Known(vec![])].map(|w| (w, vec![0.0, 1.0])));
                    for (wall, lums) in cases {
                        let g = ground(&t, style, env, &wall);
                        let what = format!("light {light} {f:?} {style:?} {wall:?}");
                        if !g.backdrop {
                            assert_eq!(g.deepen, 0.0, "{what}: an opaque plate");
                            continue;
                        }
                        // The plate itself (final sheet 1): body text's
                        // 4.5:1.
                        let need = MIN_CONTRAST;
                        let worst = |d: f32| lums.iter().copied().flat_map(|l| plates_over(&t, style, g.tint, l, d)).map(|p| bar_contrast(g.ink, p)).fold(f32::INFINITY, f32::min);
                        assert!(worst(g.deepen) >= need, "{what}: {:.2} at {}", worst(g.deepen), g.deepen);
                        if g.deepen > 0.0 {
                            deepened = true;
                            // No denser than it must be.
                            assert!(worst(g.deepen - 1.0 / DEEPEN_STEPS as f32) < need, "{what}: {}", g.deepen);
                            assert_eq!(g.laid(g.tint).a, crate::glass::fold(g.tint, Rgba { a: (g.deepen * 255.0).round() as u8, ..g.tint }).a);
                        } else {
                            assert_eq!(g.laid(g.tint), g.tint);
                        }
                    }
                    // Not known is the worst known: as dense as a wallpaper
                    // black and white at once, never less.
                    let unknown = ground(&t, style, env, &Wall::Pending).deepen;
                    let both = ground(&t, style, env, &Wall::Known(vec![Col { mean: 0.5, lo: 0.0, hi: 1.0 }])).deepen;
                    assert_eq!(unknown, both, "light {light} {f:?} {style:?}");
                    for c in &walls {
                        assert!(unknown >= ground(&t, style, env, &Wall::Known(c.clone())).deepen, "light {light} {f:?} {style:?} {c:?}");
                    }
                }
                for style in [Style::Clear, Style::Solid] {
                    assert_eq!(ground(&t, style, env, &Wall::Known(walls[4].clone())).deepen, 0.0);
                    assert_eq!(ground(&t, style, env, &Wall::Unknown { retry: false }).deepen, 0.0);
                }
            }
        }
        assert!(!deepened, "the dense Windows frost already keeps its contrast");
        // The audit's case: the dark frost over white.
        let mut t = Theme::graphite();
        t.finish = Finish::Glass;
        let t = t.resolve_fx(&SysLook { light: false, ..Default::default() }, true);
        assert_eq!(ground(&t, Style::Frost, env, &Wall::Known(vec![Col::flat(1.0)])).deepen, 0.0);
        // The review's case: the same bar over a slideshow (or a picture
        // that cannot be read) is no lighter than over white.
        let white = ground(&t, Style::Frost, env, &Wall::Known(vec![Col::flat(1.0)])).deepen;
        for w in [Wall::Unknown { retry: false }, Wall::Unknown { retry: true }, Wall::Pending] {
            assert!(ground(&t, Style::Frost, env, &w).deepen >= white, "{w:?}");
        }
        // The bar asks for the wallpaper on every see-through style.
        assert!(reads_wall(Style::Clear) && reads_wall(Style::Frost) && reads_wall(Style::Recipe) && !reads_wall(Style::Solid));
        let src = include_str!("mod.rs");
        assert!(src.contains("let t = ground.laid(t);"), "apply_theme lays the denser tint");
    }

    #[test]
    fn each_style_stands_on_its_own_ground() {
        let env = Env::default();
        for light in [false, true] {
            for t in themes(light) {
                let what = format!("light {light} {:?}", t.finish);
                // Clear: no plate, no blur; the ink and its halo follow
                // the wallpaper, the theme's mode while it is not known.
                for (wall, black) in [(Wall::Known(vec![Col::flat(0.9)]), true), (Wall::Known(vec![Col::flat(0.02)]), false), (Wall::Pending, light)] {
                    let g = ground(&t, Style::Clear, env, &wall);
                    assert_eq!((g.tint.a, g.backdrop, g.style), (0, false, Style::Clear), "{what}");
                    assert_eq!(g.ink, if black { INK_ON_LIGHT } else { INK_ON_DARK }, "{what} {wall:?}");
                    assert_eq!(g.halo, Some(if black { HALO_UNDER_BLACK } else { HALO_UNDER_WHITE }));
                    assert_eq!(g.mark, Some(if black { MARK_ON_LIGHT } else { MARK_ON_DARK }));
                }
                // Frost: one plate whatever the recipe, over the blur.
                let f = ground(&t, Style::Frost, env, &Wall::Pending);
                assert_eq!((f.tint, f.backdrop), (if light { FROST_LIGHT } else { FROST_DARK }, true), "{what}");
                assert_eq!(f.ink, t.topbar_text());
                // Recipe: the material's M0 (Glass: over the blur; Flat:
                // opaque).
                let r = ground(&t, Style::Recipe, env, &Wall::Pending);
                assert_eq!(r.backdrop, t.glass(), "{what}");
                assert_eq!(r.tint.a == 255, !t.glass());
                // Solid: Flat v2's M0 in any finish (its fall, its mean as
                // the tint), opaque.
                let s = ground(&t, Style::Solid, env, &Wall::Known(vec![Col::flat(0.9)]));
                let m = crate::theme::FLAT[light as usize][crate::theme::Material::Bar as usize].mean();
                assert_eq!((s.tint, s.backdrop, s.ink, s.halo), (Rgba { r: m, g: m, b: m, a: 255 }, false, t.topbar_text(), None));
                assert_eq!(s.fall, t.bar_band());
                assert!(s.fall.is_some());
                // The open cell: white .18 dark, black .10 light.
                for g in [f, r, s] {
                    assert_eq!(g.mark, Some(if light { MARK_ON_LIGHT } else { MARK_ON_DARK }), "{what}");
                }
            }
        }
    }

    #[test]
    fn solid_is_flats_bar_and_high_contrast_or_no_transparency_is_solid() {
        for light in [false, true] {
            let [flat, glass] = themes(light);
            // Solid is Flat's material M0 exactly.
            assert_eq!(ground(&glass, Style::Solid, Env::default(), &Wall::Pending).tint, Rgba { a: 255, ..flat.material(crate::theme::Material::Bar) });
            let off = Env { transparency: false, ..Env::default() };
            for style in [Style::Clear, Style::Frost, Style::Recipe, Style::Solid] {
                assert_eq!(in_force(style, &glass, off), Style::Solid, "{style:?}");
            }
            // Frost needs a blur; the others do not.
            let no_blur = Env { blur: false, ..Env::default() };
            assert_eq!(in_force(Style::Frost, &glass, no_blur), Style::Solid);
            assert_eq!(in_force(Style::Clear, &glass, no_blur), Style::Clear);
            let hc = HighContrast { window: Rgba { r: 0, g: 0, b: 0, a: 255 }, text: Rgba { r: 255, g: 255, b: 255, a: 255 }, highlight: Rgba { r: 255, g: 255, b: 0, a: 255 }, highlight_text: Rgba { r: 0, g: 0, b: 0, a: 255 }, gray_text: crate::syslook::GRAY_TEXT };
            let t = Theme::graphite().resolve_fx(&SysLook { light, high_contrast: Some(hc), ..Default::default() }, true);
            for style in [Style::Clear, Style::Frost, Style::Recipe, Style::Solid] {
                let g = ground(&t, style, Env::default(), &Wall::Known(vec![Col::flat(1.0)]));
                assert_eq!((g.style, g.tint, g.ink, g.mark, g.halo), (Style::Solid, hc.window, hc.text, None, None), "{style:?}");
                assert!(g.edge.is_some(), "high contrast's line along the bar");
            }
        }
    }

    /// The final review (2026-11-11, P2 1): the frost bar falls back by
    /// the rule every glass does. Where the device (WARP) or Low-spec mode
    /// takes the blur away, no style stands on a blur, the theme resolved
    /// the way the app resolves it (Theme::resolve: glass_blur); a Flat
    /// finish the owner picked keeps the frost bar over a real blur.
    #[test]
    fn frost_falls_back_with_every_other_glass() {
        let mut cfg = crate::config::Config::default();
        cfg.effects.blur = true;
        cfg.effects.low_spec = false;
        let normal = Env::of(&cfg, true, false);
        let warp = Env::of(&cfg, true, true);
        cfg.effects.low_spec = true;
        let low_spec = Env::of(&cfg, true, false);
        cfg.effects.low_spec = false;
        cfg.effects.blur = false;
        let blur_off = Env::of(&cfg, true, false);
        assert_eq!(normal, Env::default());
        assert!(normal.blurs() && !warp.blurs() && !low_spec.blurs() && !blur_off.blurs());
        for light in [false, true] {
            let sys = SysLook { light, ..Default::default() };
            for f in [Finish::Flat, Finish::Glass] {
                let mut raw = Theme::graphite();
                raw.finish = f;
                for (name, env) in [("normal", normal), ("warp", warp), ("low spec", low_spec), ("blur off", blur_off)] {
                    let t = raw.resolve_fx(&sys, crate::theme::glass_blur(env.blur, env.software));
                    for style in [Style::Clear, Style::Frost, Style::Recipe, Style::Solid] {
                        let g = ground(&t, style, env, &Wall::Known(vec![Col::flat(0.5)]));
                        let what = format!("{name} {f:?} {style:?} light {light}");
                        if env.blurs() {
                            assert_eq!(g.style, style, "{what}");
                            assert_eq!(g.backdrop, style == Style::Frost || (style == Style::Recipe && f != Finish::Flat), "{what}");
                        } else {
                            // Nothing over a blur that is not there; frost
                            // is the opaque band, like Recipe's Flat.
                            assert!(!g.backdrop, "{what}");
                            assert_eq!(g.style, if style == Style::Frost { Style::Solid } else { style }, "{what}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn clear_ink_keeps_the_better_contrast() {
        // At the crossover both inks read alike; either side the better one
        // wins.
        let white = Rgba { r: 255, g: 255, b: 255, a: 255 };
        let black = Rgba { r: 0, g: 0, b: 0, a: 255 };
        for v in [0u8, 60, 100, 118, 119, 140, 200, 255] {
            let bg = Rgba { r: v, g: v, b: v, a: 255 };
            let l = crate::theme::luminance(bg);
            let black_wins = crate::theme::contrast(black, bg) > crate::theme::contrast(white, bg);
            assert_eq!(light_over(Some(l), false), black_wins, "grey {v}");
        }
        assert!(light_over(None, true) && !light_over(None, false), "unknown: the theme's mode");
    }

    /// The review (2026-11-11, P2): a wallpaper its file cannot tell (a
    /// slideshow, a picture that cannot be read or placed) is Unknown,
    /// and clear stands on Solid over it: the theme's ink on an opaque
    /// band, readable over anything. A failed read is tried again three
    /// times; a slideshow is not (its file is not what is shown). Only a
    /// plain desktop is its color.
    #[test]
    fn a_wallpaper_that_cannot_be_known_is_solid_and_tried_again() {
        let mon = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
        let bar = RECT { bottom: 26, ..mon };
        let pic = |w: u32, h: u32, v: u8| crate::icons::Icon { width: w, height: h, pixels: [v, v, v, 255].repeat((w * h) as usize) };
        // A slideshow: whatever its file says.
        assert_eq!(judged(true, Ok(Desktop::Picture(pic(64, 36, 255))), mon, bar), Wall::Unknown { retry: false });
        // A read that failed (decoding, placing, COM), a picture with no
        // pixels: tried again.
        assert_eq!(judged(false, Ok(Desktop::Unreadable), mon, bar), Wall::Unknown { retry: true });
        assert_eq!(judged(false, Err(()), mon, bar), Wall::Unknown { retry: true });
        assert_eq!(judged(false, Ok(Desktop::Picture(pic(0, 0, 255))), mon, bar), Wall::Unknown { retry: true });
        // A plain desktop: its color, one column; a picture: its columns.
        assert_eq!(judged(false, Ok(Desktop::Plain([0, 0, 0])), mon, bar), Wall::Known(vec![Col::flat(0.0)]));
        assert_eq!(judged(false, Ok(Desktop::Picture(pic(64, 36, 255))), mon, bar), Wall::Known(vec![Col::flat(1.0); 64]));
        // Tried again after 2, 10 and 60 s, then not; the others never.
        let failed = Wall::Unknown { retry: true };
        assert_eq!((0..4).map(|k| retry_after(&failed, k)).collect::<Vec<_>>(), [Some(2_000), Some(10_000), Some(60_000), None]);
        for w in [Wall::Unknown { retry: false }, Wall::Pending, Wall::Known(vec![Col::flat(0.5)])] {
            assert_eq!(retry_after(&w, 0), None, "{w:?}");
        }
        for light in [false, true] {
            for t in themes(light) {
                let g = ground(&t, Style::Clear, Env::default(), &Wall::Unknown { retry: true });
                assert_eq!((g.style, g.tint.a, g.backdrop, g.halo, g.ink), (Style::Solid, 255, false, None, t.topbar_text()), "light {light}");
                assert!(crate::theme::contrast(crate::theme::over(g.ink, g.tint), g.tint) >= MIN_CONTRAST, "readable on its band");
                // While it is being read: the theme's mode, still clear.
                assert_eq!(ground(&t, Style::Clear, Env::default(), &Wall::Pending).style, Style::Clear);
            }
        }
    }

    /// The review (2026-11-11, P2): one mean for the whole bar put black
    /// text over the black part of a mostly white strip. Each cell takes
    /// the ink of the columns right under it, and where those are too
    /// mixed for either ink (stripes, a mid grey) a guard plate goes under
    /// it; the text then keeps MIN_CONTRAST over the darkest and the
    /// brightest of them, with the real inks (black .86, white .92) and
    /// the plate blended as the compositor blends them.
    #[test]
    fn each_cell_keeps_its_contrast_over_what_is_under_it() {
        let reads = |l: &CellLook, under: f32| ink_contrast(l.ink, l.guard, under);
        for drawn in [INK_ON_LIGHT, INK_ON_DARK] {
            // White under it: black, no plate; black under it: white.
            let w = cell_look(&[1.0; 12], None, drawn).unwrap();
            assert_eq!((shown_ink(INK_ON_LIGHT, drawn), w.halo, w.mark, w.guard), (w.ink, HALO_UNDER_BLACK, MARK_ON_LIGHT, None));
            let b = cell_look(&[0.0; 12], None, drawn).unwrap();
            assert_eq!((b.ink.r, b.halo, b.mark, b.guard), (255, HALO_UNDER_WHITE, MARK_ON_DARK, None));
            assert!(reads(&w, 1.0) >= MIN_CONTRAST && reads(&b, 0.0) >= MIN_CONTRAST);
            // A strip mostly white with a black block where the clock is:
            // the bar's mean asks black, the clock's cell white.
            let mut strip = vec![1.0f32; 100];
            strip.extend([0.0f32; 20]);
            assert!(Wall::Known(strip.iter().map(|&l| Col::flat(l)).collect()).mean().unwrap() > CROSSOVER, "the bar is light");
            let clock = cell_look(cols_under(&strip, 720.0, 640.0, 72.0), None, drawn).unwrap();
            assert_eq!((clock.ink.r, clock.guard), (255, None), "white over the black block");
            assert!(reads(&clock, 0.0) >= MIN_CONTRAST);
            // Black and white stripes, a mid grey, and a cell whose ink is
            // fixed (the battery) over the wrong ground: a plate, and the
            // text reads over every column.
            let stripes: Vec<f32> = (0..24).map(|k| if k % 2 == 0 { 0.0 } else { 1.0 }).collect();
            for (cols, fixed) in [(&stripes[..], None), (&[0.18f32; 6][..], None), (&[0.0f32; 6][..], Some(true)), (&[1.0f32; 6][..], Some(false))] {
                let l = cell_look(cols, fixed, drawn).unwrap();
                let g = l.guard.expect("a guard plate");
                for &under in cols {
                    assert!(reads(&l, under) >= MIN_CONTRAST, "{cols:?} {fixed:?}: {} over {under}", reads(&l, under));
                }
                // As see-through as that allows: a step less is not enough.
                let less = Rgba { a: g.a.saturating_sub(13), ..g };
                assert!(cols.iter().any(|&u| ink_contrast(l.ink, Some(less), u) < MIN_CONTRAST) || g.a <= 13, "{cols:?}: {}", g.a);
                assert_eq!(g.r, if l.ink.r == 0 { SOLID_LIGHT.r } else { SOLID_DARK.r }, "the opposite of the ink");
                if let Some(f) = fixed {
                    assert_eq!(l.ink, shown_ink(if f { INK_ON_LIGHT } else { INK_ON_DARK }, drawn), "fixed");
                }
            }
        }
        // Another ink than drawn shows through the content's mask at its
        // own strength (no stronger than the drawing).
        assert_eq!(mask_ink(INK_ON_LIGHT, INK_ON_DARK).a, 238);
        assert_eq!(shown_ink(INK_ON_LIGHT, INK_ON_DARK), INK_ON_LIGHT);
        assert_eq!((mask_ink(INK_ON_DARK, INK_ON_LIGHT).a, shown_ink(INK_ON_DARK, INK_ON_LIGHT).a), (255, 219));
        assert_eq!(mask_ink(INK_ON_LIGHT, INK_ON_LIGHT), Rgba { a: 255, ..INK_ON_LIGHT });
        assert_eq!(cell_look(&[], None, INK_ON_LIGHT), None);
        // The columns under a span of the bar (at least one).
        let cols: Vec<f32> = (0..10).map(|k| k as f32).collect();
        assert_eq!(cols_under(&cols, 100.0, 0.0, 20.0), &[0.0, 1.0]);
        assert_eq!(cols_under(&cols, 100.0, 95.0, 30.0), &[9.0]);
        assert_eq!(cols_under(&cols, 100.0, 41.0, 0.5), &[4.0]);
        assert!(cols_under::<f32>(&[], 100.0, 0.0, 10.0).is_empty());
    }

    /// The review (2026-11-11, P2): the bar's own rectangle is measured,
    /// not the monitor's edge down to the bar's foot: a taskbar the bar
    /// steps around (on any edge) is not under it, nor the width a side
    /// taskbar takes. On a picture of the monitor shrunk to 640 px, on a
    /// monitor left of and above the main one, at 150 %.
    #[test]
    fn the_bars_own_rectangle_is_measured() {
        let mon = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
        let (pw, ph) = super::super::about::canvas_size(1920, 1080);
        assert_eq!((pw, ph), (640, 360));
        // Plain, a taskbar 48 px on top, one 60 px on the left, one on
        // the right, and the bar at the bottom above a bottom taskbar.
        let at = |l, t, r, b| RECT { left: l, top: t, right: r, bottom: b };
        assert_eq!(bar_crop(mon, at(0, 0, 1920, 26), pw, ph), Some((0, 0, 640, 9)));
        assert_eq!(bar_crop(mon, at(0, 48, 1920, 74), pw, ph), Some((0, 16, 640, 25)));
        assert_eq!(bar_crop(mon, at(60, 0, 1920, 26), pw, ph), Some((20, 0, 640, 9)));
        assert_eq!(bar_crop(mon, at(0, 0, 1860, 26), pw, ph), Some((0, 0, 620, 9)));
        assert_eq!(bar_crop(mon, at(0, 1006, 1920, 1032), pw, ph), Some((0, 335, 640, 344)));
        // Left of and above the main display; at 150 % on 4K.
        let left = at(-1920, -200, 0, 880);
        assert_eq!(bar_crop(left, at(-1920, -200, 0, -174), pw, ph), Some((0, 0, 640, 9)));
        let k4 = at(0, 0, 3840, 2160);
        assert_eq!(bar_crop(k4, at(0, 0, 3840, 39), 640, 360), Some((0, 0, 640, 7)));
        assert_eq!(bar_crop(mon, at(2000, 0, 2500, 26), pw, ph), None, "not on it");
        // The review's case: white where the taskbar is (y 0..48), black
        // under the bar (48..74): the bar's cells are white ink.
        let px: Vec<u8> = (0..ph).flat_map(|y| { let v = if y < 16 { 255u8 } else { 0 }; [v, v, v, 255].repeat(pw as usize) }).collect();
        let cols = columns(&px, pw, ph, bar_crop(mon, at(0, 48, 1920, 74), pw, ph).unwrap());
        assert_eq!(cols.len(), 640);
        assert!(cols.iter().all(|&c| c == Col::flat(0.0)));
        assert_eq!(cell_look(&extremes(&cols), None, INK_ON_LIGHT).unwrap().ink.r, 255);
        let whole = columns(&px, pw, ph, (0, 0, pw, 25));
        assert!(whole[0].mean > CROSSOVER, "from the monitor's edge it read light");
        assert!(columns(&px, pw, ph, (0, 0, pw + 1, 9)).is_empty() && columns(&px, pw, ph, (4, 4, 4, 9)).is_empty());
    }

    /// The review (2026-11-11, P2): a spanned wallpaper's part on the main
    /// display changes when only another display does. The displays' box
    /// is in the key (and WM_DISPLAYCHANGE makes every measurement
    /// stale), so the same monitor and bar are measured again.
    #[test]
    fn another_display_changing_measures_a_spanned_wallpaper_again() {
        let main = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
        let bar = RECT { bottom: 26, ..main };
        // The second display right of the main one at 1920 x 1080, then
        // moved to its left and made 2560 x 1440.
        let before = RECT { left: 0, top: 0, right: 3840, bottom: 1080 };
        let after = RECT { left: -2560, top: -360, right: 1920, bottom: 1080 };
        assert_ne!(key(main, bar, before, false), key(main, bar, after, false));
        assert_eq!(key(main, bar, before, false), key(main, bar, before, false));
        // The main display's part of a 5120 x 1440 picture is another.
        let (a, b) = (super::super::about::span_src(5120, 1440, before, main), super::super::about::span_src(5120, 1440, after, main));
        assert_ne!(a, b, "{a:?} {b:?}");
        // A display change: stale.
        let k = key(main, bar, before, false);
        wallpaper_changed();
        assert_ne!(key(main, bar, before, false), k);
        let src = include_str!("mod.rs");
        let at = src.find("if msg == WM_DISPLAYCHANGE {").expect("the display change");
        assert!(src[at..at + 400].contains("look::wallpaper_changed()"));
    }

    /// The answers come back by token, once.
    #[test]
    fn an_answer_is_taken_once() {
        let measured = Wall::Known(vec![Col::flat(0.5)]);
        ANSWERS.lock().unwrap().push((u64::MAX - 7, WallRead::Standard(measured.clone())));
        assert!(matches!(taken(u64::MAX - 7), Some(WallRead::Standard(wall)) if wall == measured));
        assert!(taken(u64::MAX - 7).is_none());
        assert_eq!(Wall::Known(vec![Col::flat(0.0), Col::flat(1.0)]).mean(), Some(0.5));
        assert_eq!((Wall::Pending.mean(), Wall::Known(vec![]).mean()), (None, None));
        assert_eq!(grey(0.0), 0);
        assert_eq!(grey(1.0), 255);
        assert!((crate::theme::luminance(Rgba { r: grey(0.18), g: grey(0.18), b: grey(0.18), a: 255 }) - 0.18).abs() < 0.005);
    }

    /// The confirm review (2026-11-11, P2): each column's mean down the bar
    /// hid a band across it. Nine rows, the middle three black and the
    /// rest white, read 2/3 in every column: black text and no plate, and
    /// black text over the black rows is 1:1. Through the real path (BGRA
    /// -> columns -> the cell's columns -> extremes -> cell_look), the text
    /// keeps MIN_CONTRAST over every row of what is under it. Also over a
    /// one-row line, and a bright band on a dark strip.
    #[test]
    fn a_band_across_the_bar_is_under_the_text_too() {
        let (w, h) = (48u32, 9u32);
        let picture = |row: &dyn Fn(u32) -> u8| -> Vec<u8> { (0..h).flat_map(|y| { let v = row(y); [v, v, v, 255].repeat(w as usize) }).collect() };
        let cases: [(&str, fn(u32) -> u8); 4] = [
            ("black band on white", |y| if (3..6).contains(&y) { 0 } else { 255 }),
            ("white band on black", |y| if (3..6).contains(&y) { 255 } else { 0 }),
            ("one dark line", |y| if y == 4 { 20 } else { 250 }),
            ("plain white", |_| 255),
        ];
        for (name, row) in cases {
            let px = picture(&row);
            let cols = columns(&px, w, h, (0, 0, w, h));
            assert_eq!(cols.len(), w as usize);
            let rows: Vec<f32> = (0..h).map(|y| { let v = row(y); crate::theme::luminance(Rgba { r: v, g: v, b: v, a: 255 }) }).collect();
            let mean = rows.iter().sum::<f32>() / h as f32;
            assert!(cols.iter().all(|c| (c.mean - mean).abs() < 1e-5), "{name}: the mean is kept");
            for drawn in [INK_ON_LIGHT, INK_ON_DARK] {
                let l = cell_look(&extremes(cols_under(&cols, 48.0, 8.0, 24.0)), None, drawn).unwrap();
                for (y, &under) in rows.iter().enumerate() {
                    let c = ink_contrast(l.ink, l.guard, under);
                    assert!(c >= MIN_CONTRAST, "{name}: row {y} ({under}) reads {c}");
                }
                if name == "plain white" {
                    assert_eq!(l.guard, None, "nothing mixed: no plate");
                } else {
                    assert!(l.guard.is_some(), "{name}: a plate");
                }
            }
        }
        // What the review computed: the old mean alone picked black and
        // no plate (2/3 white reads light).
        let px = picture(&|y| if (3..6).contains(&y) { 0 } else { 255 });
        let cols = columns(&px, w, h, (0, 0, w, h));
        let means: Vec<f32> = cols.iter().map(|c| c.mean).collect();
        let old = cell_look(&means, None, INK_ON_LIGHT).unwrap();
        assert_eq!((old.ink.r, old.guard), (0, None));
        assert!(ink_contrast(old.ink, old.guard, 0.0) < 1.1, "the review's 1:1");
        // A line along the bar's very edge is under no text: no plate.
        let px = picture(&|y| if y == 0 || y == h - 1 { 0 } else { 255 });
        let edge = columns(&px, w, h, (0, 0, w, h));
        assert_eq!(cell_look(&extremes(&edge), None, INK_ON_LIGHT).unwrap().guard, None);
        assert_eq!((ink_rows(0, 9), ink_rows(0, 26), ink_rows(4, 5), ink_rows(0, 2)), ((1, 8), (3, 23), (4, 5), (0, 2)));
        assert_eq!(extremes(&[]), Vec::<f32>::new());
        assert_eq!(cell_look(&extremes(&[]), None, INK_ON_LIGHT), None);
        // The bar reads a larger picture than the About panel's, so a
        // band a few pixels high is not shrunk into the rows around it.
        use super::super::about::{canvas_size, canvas_within, BAR_PICTURE};
        assert_eq!(canvas_within(1920, 1080, BAR_PICTURE), (1280, 720));
        assert_eq!(canvas_within(1280, 720, BAR_PICTURE), (1280, 720), "never grown");
        assert_eq!(canvas_within(1920, 1080, 640), canvas_size(1920, 1080));
        let mon = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
        let (pw, ph) = canvas_within(1920, 1080, BAR_PICTURE);
        assert_eq!(bar_crop(mon, RECT { bottom: 26, ..mon }, pw, ph), Some((0, 0, 1280, 18)), "18 rows under a 26 px bar");
    }

    /// The confirm review (2026-11-11, P2): a picture that is set but whose
    /// file is gone, or whose lookup failed, came out as no path and so as
    /// a plain color: known, clear, never tried again. Only "no picture"
    /// from every lookup is the plain color; everything else is unreadable,
    /// solid and tried again. Through the real classification (the
    /// lookups' answers -> the desktop -> what it shows -> judged).
    #[test]
    fn a_lost_wallpaper_file_is_not_a_plain_color() {
        use super::super::about::{desktop_of, Named};
        let mon = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
        let bar = RECT { bottom: 26, ..mon };
        let there = |p: &std::path::Path| p.to_str() == Some("C:/there.jpg");
        let back = Some([250, 250, 250]);
        // (the monitor's lookup, Windows' one setting, plain?)
        let cases: [(Option<&str>, Option<&str>, Option<bool>); 9] = [
            (Some(""), Some(""), Some(true)),
            (Some("C:/gone.jpg"), Some("C:/gone.jpg"), Some(false)),
            (Some("C:/gone.jpg"), Some(""), Some(false)),
            (None, Some(""), Some(false)),
            (None, None, Some(false)),
            (Some(""), None, Some(false)),
            (Some(""), Some("C:/gone.jpg"), Some(false)),
            (Some("C:/gone.jpg"), Some("C:/there.jpg"), None),
            (Some("C:/there.jpg"), None, None),
        ];
        for (mine, system, plain) in cases {
            let named = Named::of(mine, there).or(|| Named::of(system, there));
            let desk = named.desk(None, back);
            let pic = crate::icons::Icon { width: 64, height: 36, pixels: [255, 255, 255, 255].repeat(64 * 36) };
            let shown = desktop_of(&desk, |d| d.path.is_some().then_some(pic));
            let wall = judged(false, Ok(shown), mon, bar);
            match plain {
                Some(true) => assert_eq!(wall, Wall::Known(vec![Col::flat(crate::theme::luminance(Rgba { r: 250, g: 250, b: 250, a: 255 }))]), "{mine:?} {system:?}"),
                Some(false) => {
                    assert_eq!(wall, Wall::Unknown { retry: true }, "{mine:?} {system:?}");
                    assert_eq!(retry_after(&wall, 0), Some(RETRY_MS[0]), "{mine:?} {system:?}: tried again");
                }
                None => assert_eq!(wall, Wall::Known(vec![Col::flat(1.0); 64]), "{mine:?} {system:?}: the file found"),
            }
        }
        // Only the second lookup when the first has no file.
        let mut asked = false;
        assert_eq!(Named::of(Some("C:/there.jpg"), there).or(|| { asked = true; Named::Nothing }), Named::File("C:/there.jpg".into()));
        assert!(!asked);
        // A set picture that cannot be read stays unreadable; no color
        // known at all is unreadable too.
        let found = Named::File("C:/there.jpg".into()).desk(None, back);
        assert!(matches!(desktop_of(&found, |_| None), Desktop::Unreadable));
        assert!(matches!(desktop_of(&Named::Nothing.desk(None, None), |_| None), Desktop::Unreadable));
        // The confirm review 2 (P2): the monitor itself could not be asked
        // (IDesktopWallpaper failed or does not know it). Windows' empty
        // setting does not tell what that monitor shows: unreadable, tried
        // again; a file the setting names is still used.
        use super::super::about::unasked;
        for (global, want) in [(Some(""), None), (None, None), (Some("C:/gone.jpg"), None), (Some("C:/there.jpg"), Some(true))] {
            let named = unasked(global, there);
            let desk = named.desk(None, back);
            let pic = crate::icons::Icon { width: 64, height: 36, pixels: [255, 255, 255, 255].repeat(64 * 36) };
            let wall = judged(false, Ok(desktop_of(&desk, |d| d.path.is_some().then_some(pic))), mon, bar);
            match want {
                None => {
                    assert_eq!(wall, Wall::Unknown { retry: true }, "{global:?}");
                    assert_eq!(retry_after(&wall, 0), Some(RETRY_MS[0]), "{global:?}: tried again");
                }
                Some(_) => assert_eq!(wall, Wall::Known(vec![Col::flat(1.0); 64]), "{global:?}: the file found"),
            }
        }
    }

    /// The confirm review (2026-11-11, P3): the cells' looks are read
    /// again only when the wallpaper, the ground's ink or the cells' places
    /// change, never for a hover or a press (not in what they are read
    /// from), and a guard plate sets only what changed, its brush kept.
    #[test]
    fn the_cells_looks_are_read_again_only_when_what_is_under_them_changes() {
        let mut stripes: Vec<Col> = (0..24).map(|k| Col::flat(if k % 2 == 0 { 0.0 } else { 1.0 })).collect();
        stripes.extend(vec![Col::flat(1.0); 24]);
        let wall = Wall::Known(stripes);
        let k = |ver: u64, x: f32| LooksIn { wall: ver, style: Style::Clear, ink: INK_ON_LIGHT, width: 480.0, cells: vec![Some((x, 60.0, None)), Some((300.0, 60.0, Some(true))), None] };
        let mut c = CellLooks::default();
        assert!(c.refresh(&wall, k(1, 10.0)));
        assert_eq!(c.reads, 1);
        assert!(c.get(0).and_then(|l| l.guard).is_some() && c.get(1).is_some() && c.get(2).is_none());
        // The same wallpaper and places (a hover moving, the clock
        // ticking at the same width): not read again.
        for _ in 0..50 {
            assert!(!c.refresh(&wall, k(1, 10.0)));
        }
        assert_eq!(c.reads, 1);
        // A cell moved, a new measurement, another ink, another style:
        // read again (changed only if a look did).
        assert!(c.refresh(&wall, k(1, 300.0)));
        assert_eq!(c.reads, 2);
        assert!(!c.refresh(&wall, k(2, 300.0)), "the same looks");
        assert_eq!(c.reads, 3);
        assert!(c.refresh(&wall, LooksIn { style: Style::Solid, ..k(2, 300.0) }));
        assert_eq!(c.looks, vec![None, None, None]);
        assert_eq!(c.reads, 4);
        assert!(!c.refresh(&Wall::Pending, LooksIn { wall: 3, ..k(2, 300.0) }), "not known: none, as before");
        // The guard plate: only what changed.
        let spot = (10.0, 2.0, 60.0, 22.0, 6.0);
        let (a, b) = (Rgba { a: 140, ..SOLID_LIGHT }, Rgba { a: 166, ..SOLID_LIGHT });
        assert_eq!(plate_edit(None, None), PlateEdit::default());
        assert_eq!(plate_edit(None, Some((spot, a))), PlateEdit { place: true, color: true, show: Some(true) });
        assert_eq!(plate_edit(Some((spot, a)), Some((spot, a))), PlateEdit::default(), "a hover: nothing");
        assert_eq!(plate_edit(Some((spot, a)), Some((spot, b))), PlateEdit { color: true, ..Default::default() });
        let moved = (40.0, 2.0, 60.0, 22.0, 6.0);
        assert_eq!(plate_edit(Some((spot, a)), Some((moved, a))), PlateEdit { place: true, ..Default::default() });
        assert_eq!(plate_edit(Some((spot, a)), None), PlateEdit { show: Some(false), ..Default::default() });
        // TopBar reads them through CellLooks and sets plates through
        // plate_edit, recoloring the brush it keeps.
        let src = include_str!("mod.rs");
        let at = src.find("fn layout_hover(&mut self)").expect("layout_hover");
        let body = &src[at..at + 1600];
        assert!(body.contains("self.cell_looks.refresh(") && !body.contains("cell_looks_now"), "{body}");
        let g = src.find("impl Guards {").expect("Guards");
        let guards = &src[g..g + 3000];
        assert!(guards.contains("look::plate_edit(") && guards.contains(".SetColor("), "{guards}");
    }
}

#[cfg(test)]
mod brand_review_tests {
    use super::*;

    #[test]
    fn brand_mark_follows_ground_for_every_style() {
        for light in [false, true] {
            let t = Theme::graphite().resolve_fx(&crate::syslook::SysLook { light, ..Default::default() }, true);
            for style in [Style::Solid, Style::Frost, Style::Recipe] {
                let g = ground(&t, style, Env::default(), &Wall::Pending);
                assert_eq!(g.mark_light, light, "{style:?}");
            }
            for wall_light in [false, true] {
                let wall = Wall::Known(vec![Col::flat(if wall_light { 1.0 } else { 0.0 })]);
                let g = ground(&t, Style::Clear, Env::default(), &wall);
                assert_eq!(g.mark_light, wall_light, "wallpaper overrides theme");
            }
        }
    }
}
