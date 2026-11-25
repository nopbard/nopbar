//! The pill and the card as one plate (design system v2.1, 7.2 rule 4 and
//! 7.4): one thing that changes size, corners, material, shadow and rim
//! on one progress `p` (0 the pill, 1 the card), with the pill's content
//! leaving over the first SPLIT of the way and the card's arriving over
//! the rest.
//!
//! How it is built. `p` is a scalar in a property set, driven by one GLIDE
//! spring (crate::motion::spring_prop: retargeted, it leaves from where it
//! is at the speed it has, so a card closed while it opens, or opened
//! again while it closes, turns round in place). Everything else is an
//! expression of `p` the compositor evaluates: the plate's rectangle and
//! corner radius (the clip of its body, the rim's and the contact ring's
//! geometry), the two tints (the pill's M4 under the card's M2), the two
//! shadows (E1 fading out as E3 fades in, each a nine-grid patch so its
//! corners keep their shape as the plate grows) and the two contents'
//! opacity. Nothing runs per frame in our process.
//!
//! The pill itself (mod.rs) wears the same E1 patch and contact ring, so
//! the plate at p = 0 is the pill, pixel for pixel: the card's window
//! opens over it and the bar's own pill is hidden behind it.

use std::time::{Duration, Instant};

use windows::core::{Interface, Result, HSTRING};
use windows::UI::Composition::{
    CompositionBrush, CompositionGeometricClip, CompositionNineGridBrush, CompositionObject, CompositionPropertySet, CompositionRoundedRectangleGeometry, CompositionStretch,
    Compositor, ContainerVisual, SpriteVisual,
};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_ALIASED;
use windows_numerics::Vector2;

use crate::gfx::Gfx;
use crate::motion::{Spring, SpringRun, SPRING_GLIDE};
use crate::theme::{Elevation, Material, Rgba, Theme};

/// The pill's content is gone by this much of the way; the card's comes
/// in over the rest.
pub const SPLIT: f32 = 0.35;
/// `p` within this of its target (and as good as still) has arrived.
const SETTLED: f32 = 0.002;

/// A rounded rectangle (DIP, in the card window's own coordinates).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shape {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub r: f32,
}

// The plate's arithmetic, pure: what the expressions below (Expr) tell
// the compositor to compute. The unit tests hold these to the design; the
// audit's pictures (card_harmony.rs) read the real visuals back at fixed
// `p` and measure them against these; the card's input (card.rs) meets
// the plate through them.
fn mix(a: f32, b: f32, p: f32) -> f32 {
    a + (b - a) * p
}

/// Pure: the plate `p` of the way from `pill` to `card`. Never less than
/// the pill (a spring turned round mid-way may dip under 0).
pub fn plate_at(pill: Shape, card: Shape, p: f32) -> Shape {
    let q = p.max(0.0);
    Shape { x: mix(pill.x, card.x, q), y: mix(pill.y, card.y, q), w: mix(pill.w, card.w, q), h: mix(pill.h, card.h, q), r: mix(pill.r, card.r, q) }
}

#[cfg_attr(not(test), allow(dead_code))]
/// Pure: the pill's content at `p`: there at 0, gone by SPLIT.
pub fn pill_alpha(p: f32) -> f32 {
    (1.0 - p.max(0.0) / SPLIT).clamp(0.0, 1.0)
}

/// Pure: the card's content at `p`: nothing until SPLIT, all there at 1.
pub fn card_alpha(p: f32) -> f32 {
    ((p - SPLIT) / (1.0 - SPLIT)).clamp(0.0, 1.0)
}

#[cfg_attr(not(test), allow(dead_code))]
/// Pure: how much of the card's material, shadow and rim shows at `p` (the
/// pill's shadow shows the rest).
pub fn card_share(p: f32) -> f32 {
    p.clamp(0.0, 1.0)
}

#[cfg_attr(not(test), allow(dead_code))]
/// Pure: where the pill's content hangs on the plate (its own x 0 is the
/// pill's middle, y 0 its top): the plate's top middle; over a bar at the
/// bottom (`up`) its bottom middle. The plate grows away from the bar's
/// edge, so the content it started with stays where the pill was.
pub fn pill_origin(plate: Shape, pill: Shape, up: bool) -> (f32, f32) {
    (plate.x + plate.w / 2.0, if up { plate.y + plate.h - pill.h } else { plate.y })
}

/// Pure: how large the card's content is drawn on the plate: as wide as
/// the plate at most (1 once the plate is the card's width). A plate still
/// narrower than the card shows all of its content across, smaller,
/// never cut at its sides (2026-11-11 final audit 3: mid-way the album
/// art touched the plate's left edge and "3:34" was cut at its right);
/// down, the plate still uncovers it as it grows.
pub fn content_scale(plate: Shape, card: Shape) -> f32 {
    if card.w <= 0.0 {
        return 1.0;
    }
    (plate.w / card.w).clamp(0.0, 1.0)
}

/// Pure: where the card's content (a `card.w` x `card.h` picture, drawn at
/// content_scale) hangs on the plate: centered, at its top (its bottom,
/// `up`). At p = 1 that is the card's own corner.
pub fn card_origin(plate: Shape, card: Shape, up: bool) -> (f32, f32) {
    let k = content_scale(plate, card);
    (plate.x + plate.w / 2.0 - card.w * k / 2.0, if up { plate.y + plate.h - card.h * k } else { plate.y })
}

/// What the plate shows at one progress: its shape, where the card's
/// content hangs on it (card_origin) and the pill it started from, all in
/// the card window's own DIP. What is seen is what the pointer meets
/// (card.rs: input goes through the same transform and clip).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Drawn {
    pub p: f32,
    pub plate: Shape,
    pub origin: (f32, f32),
    /// The content's scale (content_scale).
    pub k: f32,
    pub pill: Shape,
}

impl Drawn {
    /// Card-window DIP (`x`, `y`) in the content's own DIP, as painted.
    pub fn to_content(self, x: f32, y: f32) -> (f32, f32) {
        let k = self.k.max(1e-3);
        ((x - self.origin.0) / k, (y - self.origin.1) / k)
    }
}

/// Pure: the plate `p` of the way from `pill` to `card` as the
/// expressions draw it (`up`: over a bar at the bottom).
pub fn drawn_at(pill: Shape, card: Shape, p: f32, up: bool) -> Drawn {
    let plate = plate_at(pill, card, p);
    Drawn { p, plate, origin: card_origin(plate, card, up), k: content_scale(plate, card), pill }
}

/// Pure: (`x`, `y`) is on the rounded rectangle `s` (its corners cut).
pub fn on_shape(s: Shape, x: f32, y: f32) -> bool {
    if !(x >= s.x && x < s.x + s.w && y >= s.y && y < s.y + s.h) {
        return false;
    }
    let r = s.r.clamp(0.0, s.w.min(s.h) / 2.0);
    // The nearest point of the rectangle shrunk by the radius.
    let cx = x.clamp(s.x + r, s.x + s.w - r);
    let cy = y.clamp(s.y + r, s.y + s.h - r);
    (x - cx).powi(2) + (y - cy).powi(2) <= r * r
}

/// Pure: how long `run` takes to arrive from `now` (ms, 8 ms steps, at
/// most two seconds): when the one who started it may look again.
pub fn settle_ms(run: &SpringRun, now: Instant) -> u32 {
    (1..=250u32).map(|i| i * 8).find(|ms| run.settled(now + Duration::from_millis(*ms as u64), SETTLED)).unwrap_or(2000)
}

/// What one end of the plate wears (the pill at rest, the card's panel):
/// the pill at rest (mod.rs: pill_coat) and the plate at p = 0 wear the
/// same; the plate at p = 1 wears a Flat v2 panel's (Theme::plate_coat).
pub use crate::theme::PlateCoat as Coat;

/// What the plate is made of, read from the theme once.
#[derive(Clone, Debug, PartialEq)]
pub struct Look {
    /// The pill: material M4 (opaque on Flat, no backdrop on Glass).
    pub pill: Rgba,
    /// The card: material M2, over the backdrop when `backdrop`.
    pub card: Rgba,
    pub backdrop: bool,
    /// Flat v2's falls from top to bottom (Theme::flat_fill; None: the
    /// one color above).
    pub pill_fall: Option<(Rgba, Rgba)>,
    pub card_fall: Option<(Rgba, Rgba)>,
    /// The card's rim's stops (None: high contrast).
    pub rim: Option<[Rgba; 3]>,
    /// The pill's own rim (super::pill_rim): Flat v2's M4, a glass's own
    /// light (None: high contrast, a theme file's Flat).
    pub pill_rim: Option<[Rgba; 3]>,
    /// The rims' widths (DIP: Theme::rim_width, one device pixel or Flat
    /// v2's 1 DIP): the card's, the pill's.
    pub rim_w: f32,
    pub pill_rim_w: f32,
    /// Flat v2's grain on the card (the Flat one chose; 0: none). A
    /// glass's top light is left to the panels: the morphing shape has
    /// no fixed top to light.
    pub grain: f32,
    /// High contrast's 1 DIP outline.
    pub outline: Option<Rgba>,
    pub contact: Rgba,
    /// E1 and E3 as (x, y, blur, color); empty with the "Shadows" effect
    /// off or in high contrast.
    pub e1: Vec<(f32, f32, f32, Rgba)>,
    pub e3: Vec<(f32, f32, f32, Rgba)>,
}

impl Look {
    /// On a display scaled `s`.
    pub fn of(theme: &Theme, s: f32) -> Look {
        let fx = crate::effects::items();
        Look::with(theme, fx.shadows, fx.blur, s)
    }

    /// `of`, with the "Shadows" and "Blur" effects given (pure).
    pub fn with(theme: &Theme, shadows: bool, blur: bool, s: f32) -> Look {
        let hc = theme.hc.filter(|_| theme.high_contrast);
        let card = theme.material(Material::Panel);
        let opaque = theme.popup.material == "solid" || !blur || card.a == 255;
        let layers = |e: Elevation| if shadows && hc.is_none() { theme.elevation(e) } else { Vec::new() };
        Look {
            pill: theme.material(Material::Tip),
            card: if opaque { Rgba { a: 255, ..card } } else { card },
            backdrop: !opaque,
            pill_fall: theme.flat_fill(Material::Tip),
            card_fall: theme.flat_fill(Material::Panel).filter(|_| opaque),
            rim: theme.rim(),
            pill_rim: super::pill_rim(theme),
            rim_w: theme.rim_width(Material::Panel, s),
            pill_rim_w: theme.rim_width(Material::Tip, s),
            grain: theme.flat_grain(Material::Panel),
            outline: hc.map(|h| h.text),
            contact: theme.contact(),
            e1: layers(Elevation::E1),
            e3: layers(Elevation::E3),
        }
    }
}

impl Look {
    /// The pill's rim and the card's are one (a glass: the same light,
    /// the same device pixel): the plate draws it once, the same all the
    /// way from p = 0 to p = 1.
    pub fn rim_carries(&self) -> bool {
        self.pill_rim.is_some() && self.pill_rim == self.rim && self.pill_rim_w == self.rim_w
    }

    /// What the plate wears at its two ends: (p = 0, the pill; p = 1, the
    /// card), as Plate::new draws them.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn ends(&self) -> (Coat, Coat) {
        let one = |c: Rgba| (c, c);
        let pill = Coat { fill: self.pill_fall.unwrap_or(one(self.pill)), rim: self.pill_rim.map(|r| (r, self.pill_rim_w)), grain: 0.0 };
        let card = Coat { fill: self.card_fall.unwrap_or(one(self.card)), rim: self.rim.map(|r| (r, self.rim_w)), grain: self.grain };
        (pill, card)
    }
}

fn color(c: Rgba) -> windows::UI::Color {
    windows::UI::Color { A: c.a, R: c.r, G: c.g, B: c.b }
}

/// The soft shadows `layers` cast by a rounded rectangle with corners
/// `radius`, as a nine-grid brush for a sprite `margin` DIP larger than
/// the rectangle on every side: drawn once (real Gaussian blurs,
/// gfx::soft_shadows), the corners keep their shape at any size, and its
/// inset scales turn it into the shadow of another radius (`k` times this
/// one: `inset_scale`). Its middle is hollow: nothing is laid under a
/// see-through plate.
pub fn shadow_patch(gfx: &Gfx, layers: &[(f32, f32, f32, Rgba)], radius: f32, margin: f32, s: f32) -> Result<CompositionNineGridBrush> {
    let c = &gfx.compositor;
    let inset = ((radius + margin) * s).ceil().max(1.0);
    let side = inset * 2.0 + 2.0;
    let scaled: Vec<(f32, f32, f32, Rgba)> = layers.iter().map(|&(x, y, blur, col)| (x * s, y * s, blur * s, col)).collect();
    let m = inset - radius * s;
    let surface = gfx.draw(side as u32, side as u32, |dc, ox, oy| unsafe {
        // Nothing of the blur's tail past this patch.
        dc.PushAxisAlignedClip(&D2D_RECT_F { left: ox, top: oy, right: ox + side, bottom: oy + side }, D2D1_ANTIALIAS_MODE_ALIASED);
        let r = crate::gfx::soft_shadows(dc, D2D_RECT_F { left: ox + m, top: oy + m, right: ox + side - m, bottom: oy + side - m }, radius * s, &scaled);
        dc.PopAxisAlignedClip();
        r
    })?;
    let source = c.CreateSurfaceBrushWithSurface(&surface)?;
    source.SetStretch(CompositionStretch::Fill)?;
    let nine = c.CreateNineGridBrush()?;
    nine.SetSource(&source)?;
    nine.SetInsets(inset)?;
    nine.SetIsCenterHollow(true)?;
    nine.SetInsetScales(inset_scale(1.0, s))?;
    Ok(nine)
}

/// Pure: the patch's inset scale for a shadow `k` times its own radius on
/// a display scaled `s` (its insets are in its own pixels; the sprite it
/// fills is measured in DIP).
pub fn inset_scale(k: f32, s: f32) -> f32 {
    k / s.max(0.25)
}

/// A contact ring's geometry around a `w` x `h` rounded rectangle with
/// corners `r` whose shape visual starts 1 DIP out of it (a shape visual
/// shows nothing past its size): (offset, size, corner radius, stroke).
pub fn contact_ring(w: f32, h: f32, r: f32) -> ((f32, f32), (f32, f32), f32, f32) {
    let k = crate::theme::CONTACT_W;
    ((1.0 - k / 2.0, 1.0 - k / 2.0), (w + k, h + k), r + k / 2.0, k)
}

/// An expression of the property set `m` on `prop` of `obj`.
fn bind(c: &Compositor, obj: &impl Interface, prop: &str, expr: &str, m: &CompositionPropertySet) -> Result<()> {
    let e = c.CreateExpressionAnimationWithExpression(&HSTRING::from(expr))?;
    e.SetReferenceParameter(&HSTRING::from("m"), m)?;
    obj.cast::<CompositionObject>()?.StartAnimation(&HSTRING::from(prop), &e)
}

/// The plate's expressions (of the property set `m`): its rectangle and
/// corner radius between the pill's (px, py, pw, ph, pr) and the card's
/// (cx, cy, cw, ch, cr) on `p`, as plate_at computes them.
struct Expr;

impl Expr {
    const Q: &'static str = "Max(m.p, 0)";
    const SHARE: &'static str = "Clamp(m.p, 0, 1)";

    fn x() -> String {
        format!("(m.px + (m.cx - m.px) * {})", Self::Q)
    }
    fn y() -> String {
        format!("(m.py + (m.cy - m.py) * {})", Self::Q)
    }
    fn w() -> String {
        format!("(m.pw + (m.cw - m.pw) * {})", Self::Q)
    }
    fn h() -> String {
        format!("(m.ph + (m.ch - m.ph) * {})", Self::Q)
    }
    fn r() -> String {
        format!("(m.pr + (m.cr - m.pr) * {})", Self::Q)
    }
    fn offset(dx: &str, dy: &str) -> String {
        format!("Vector3({} + {dx}, {} + {dy}, 0)", Self::x(), Self::y())
    }
    fn size(dw: &str, dh: &str) -> String {
        format!("Vector2({} + {dw}, {} + {dh})", Self::w(), Self::h())
    }
}

/// The plate, built: add `root` to the card window's tree, put the pill's
/// parts under `ghost` and the card's picture under `content`.
pub struct Plate {
    c: Compositor,
    pub root: ContainerVisual,
    /// The pill's content (x 0 its middle, y 0 its top).
    pub ghost: ContainerVisual,
    /// The card's content (its own corner at 0, 0).
    pub content: ContainerVisual,
    props: CompositionPropertySet,
    run: SpringRun,
    height: SpringRun,
    /// The ends as last put (the card's height: where it glides to).
    pill: Shape,
    card: Shape,
    up: bool,
}

impl Plate {
    /// `s`: the display's scale (the rim is one device pixel, the shadows'
    /// patches are drawn in its pixels). `up`: over a bar at the bottom.
    pub fn new(gfx: &Gfx, theme: &Theme, s: f32, pill: Shape, card: Shape, up: bool) -> Result<Plate> {
        let c = gfx.compositor.clone();
        let look = Look::of(theme, s);
        let props = c.CreatePropertySet()?;
        let now = Instant::now();
        let me = Plate {
            root: c.CreateContainerVisual()?,
            ghost: c.CreateContainerVisual()?,
            content: c.CreateContainerVisual()?,
            props,
            run: SpringRun::still(SPRING_GLIDE, 0.0, now),
            height: SpringRun::still(SPRING_GLIDE, card.h, now),
            pill,
            card,
            up,
            c,
        };
        let (c, m) = (&me.c, &me.props);
        m.InsertScalar(&HSTRING::from("p"), 0.0)?;
        me.put_pill(pill)?;
        me.put_card(card)?;
        let kids = me.root.Children()?;

        // The shadows: E1 (the pill's) going as E3 (the card's) comes.
        for (layers, e, radius, share) in [(&look.e1, Elevation::E1, "m.pr", format!("1 - {}", Expr::SHARE)), (&look.e3, Elevation::E3, "m.cr", Expr::SHARE.to_string())] {
            if layers.is_empty() {
                continue;
            }
            let (margin, own) = (e.reach(), if e == Elevation::E1 { pill.r } else { card.r });
            let patch = shadow_patch(gfx, layers, own, margin, s)?;
            // The patch as the shadow of the plate's radius now.
            let k = format!("({} / {radius})", Expr::r());
            for side in ["LeftInsetScale", "TopInsetScale", "RightInsetScale", "BottomInsetScale"] {
                bind(c, &patch, side, &format!("{k} / {s}"), m)?;
            }
            let v = c.CreateSpriteVisual()?;
            v.SetBrush(&patch)?;
            let out = format!("{margin} * {k}");
            bind(c, &v, "Offset", &Expr::offset(&format!("-{out}"), &format!("-{out}")), m)?;
            bind(c, &v, "Size", &Expr::size(&format!("2 * {out}"), &format!("2 * {out}")), m)?;
            bind(c, &v, "Opacity", &share, m)?;
            kids.InsertAtTop(&v)?;
        }

        // The contact shadow: a ring just outside the plate, all the way.
        if look.contact.a > 0 {
            let ((ox, oy), _, _, k) = contact_ring(0.0, 0.0, 0.0);
            let geom = c.CreateRoundedRectangleGeometry()?;
            geom.SetOffset(Vector2 { X: ox, Y: oy })?;
            bind(c, &geom, "Size", &Expr::size(&k.to_string(), &k.to_string()), m)?;
            bind(c, &geom, "CornerRadius", &format!("Vector2({r} + {h}, {r} + {h})", r = Expr::r(), h = k / 2.0), m)?;
            let shape = c.CreateSpriteShapeWithGeometry(&geom)?;
            shape.SetStrokeBrush(&c.CreateColorBrushWithColor(color(look.contact))?)?;
            shape.SetStrokeThickness(k)?;
            let ring = c.CreateShapeVisual()?;
            ring.Shapes()?.Append(&shape)?;
            bind(c, &ring, "Offset", &Expr::offset("-1", "-1"), m)?;
            bind(c, &ring, "Size", &Expr::size("2", "2"), m)?;
            kids.InsertAtTop(&ring)?;
        }

        // The body: everything the plate's shape cuts.
        let body = c.CreateContainerVisual()?;
        let clip = c.CreateRoundedRectangleGeometry()?;
        bind(c, &clip, "Offset", &format!("Vector2({}, {})", Expr::x(), Expr::y()), m)?;
        bind(c, &clip, "Size", &Expr::size("0", "0"), m)?;
        bind(c, &clip, "CornerRadius", &format!("Vector2({r}, {r})", r = Expr::r()), m)?;
        let clip: CompositionGeometricClip = c.CreateGeometricClipWithGeometry(&clip)?;
        body.SetClip(&clip)?;
        kids.InsertAtTop(&body)?;
        let fill = |brush: &CompositionBrush, share: Option<&str>| -> Result<SpriteVisual> {
            let v = c.CreateSpriteVisual()?;
            v.SetBrush(brush)?;
            bind(c, &v, "Offset", &Expr::offset("0", "0"), m)?;
            bind(c, &v, "Size", &Expr::size("0", "0"), m)?;
            if let Some(share) = share {
                bind(c, &v, "Opacity", share, m)?;
            }
            body.Children()?.InsertAtTop(&v)?;
            Ok(v)
        };
        // The pill's material under the card's (a backdrop, if it has one,
        // hides everything under it: at p = 1 only the card's is seen).
        // (Flat v2: each its fall from top to bottom, relative to the
        // morphing plate: Look::ends.)
        fill(&crate::glass::fall_brush(c, look.pill_fall, look.pill)?, None)?;
        // The card's glass (a panel's: crate::glass's graph over the blur,
        // its tint with the Luminosity layer folded in when the compositor
        // could not draw it). The top light is left to the panels: the
        // morphing shape has no fixed top to light.
        let card = if look.backdrop {
            let glass = crate::glass::backdrop(gfx, theme, Material::Panel)?;
            fill(&glass, Some(Expr::SHARE))?;
            crate::glass::tint_for(theme, Material::Panel, Some(&glass))
        } else {
            look.card
        };
        fill(&crate::glass::fall_brush(c, look.card_fall, card)?, Some(Expr::SHARE))?;
        // Flat v2's grain on the card (a panel's, the Flat one chose: it
        // comes with the card's material, cut by the plate's shape at any
        // size: the tile repeats, it is not stretched).
        if look.grain > 0.0 {
            if let Some(grain) = crate::glass::top_light(gfx, theme, Material::Panel, None)? {
                let host = c.CreateContainerVisual()?;
                bind(c, &host, "Offset", &Expr::offset("0", "0"), m)?;
                bind(c, &host, "Size", &Expr::size("0", "0"), m)?;
                bind(c, &host, "Opacity", Expr::SHARE, m)?;
                host.Children()?.InsertAtTop(&grain)?;
                body.Children()?.InsertAtTop(&host)?;
            }
        }

        // The two contents, each hung on the plate where its own surface
        // had it (pill_origin, card_origin).
        // The card's at content_scale: never wider than the plate.
        let k = format!("Clamp({} / Max(m.cw, 0.001), 0, 1)", Expr::w());
        let (gy, cy) = if up { (format!("{} - m.ph", Expr::h()), format!("{} - m.ch * {k}", Expr::h())) } else { ("0".to_string(), "0".to_string()) };
        bind(c, &me.ghost, "Offset", &Expr::offset(&format!("{} * 0.5", Expr::w()), &gy), m)?;
        bind(c, &me.ghost, "Opacity", &format!("Clamp(1 - {} / {SPLIT}, 0, 1)", Expr::Q), m)?;
        bind(c, &me.content, "Offset", &Expr::offset(&format!("({} - m.cw * {k}) * 0.5", Expr::w()), &cy), m)?;
        bind(c, &me.content, "Scale", &format!("Vector3({k}, {k}, 1)"), m)?;
        bind(c, &me.content, "Opacity", &format!("Clamp((m.p - {SPLIT}) / {}, 0, 1)", 1.0 - SPLIT), m)?;
        body.Children()?.InsertAtTop(&me.ghost)?;
        body.Children()?.InsertAtTop(&me.content)?;

        // The rims: the pill's (Flat v2's M4, a glass's) going as the card's comes
        // (the card's light: it comes with the card), or high contrast's
        // outline (there all the way). Each `px` DIP wide (rim_w).
        let edge = |px: f32, brush: &CompositionBrush, share: Option<String>| -> Result<()> {
            let edge = c.CreateShapeVisual()?;
            let geom: CompositionRoundedRectangleGeometry = c.CreateRoundedRectangleGeometry()?;
            geom.SetOffset(Vector2 { X: px / 2.0, Y: px / 2.0 })?;
            bind(c, &geom, "Size", &Expr::size(&format!("-{px}"), &format!("-{px}")), m)?;
            bind(c, &geom, "CornerRadius", &format!("Vector2(Max({r} - {h}, 0), Max({r} - {h}, 0))", r = Expr::r(), h = px / 2.0), m)?;
            let shape = c.CreateSpriteShapeWithGeometry(&geom)?;
            shape.SetStrokeThickness(px)?;
            shape.SetStrokeBrush(brush)?;
            if let Some(share) = share {
                bind(c, &edge, "Opacity", &share, m)?;
            }
            edge.Shapes()?.Append(&shape)?;
            bind(c, &edge, "Offset", &Expr::offset("0", "0"), m)?;
            bind(c, &edge, "Size", &Expr::size("0", "0"), m)?;
            kids.InsertAtTop(&edge)
        };
        let light = |stops: [Rgba; 3]| -> Result<CompositionBrush> {
            let light = c.CreateLinearGradientBrush()?;
            light.SetStartPoint(Vector2 { X: 0.0, Y: 0.0 })?;
            light.SetEndPoint(Vector2 { X: 0.0, Y: 1.0 })?;
            for (at, col) in crate::theme::RIM_STOPS.iter().zip(stops) {
                light.ColorStops()?.Append(&c.CreateColorGradientStopWithOffsetAndColor(*at, color(col))?)?;
            }
            light.cast()
        };
        // A glass's pill and card wear one rim: drawn once, all the way
        // (two cross-fading copies would dim it halfway).
        let one = look.rim_carries();
        if let Some(stops) = look.pill_rim.filter(|_| !one) {
            edge(look.pill_rim_w, &light(stops)?, Some(format!("1 - {}", Expr::SHARE)))?;
        }
        match (look.rim, look.outline) {
            (Some(stops), _) => edge(look.rim_w, &light(stops)?, (!one).then(|| Expr::SHARE.to_string()))?,
            (None, Some(o)) => edge(1.0, &c.CreateColorBrushWithColor(color(o))?.cast()?, None)?,
            (None, None) => {}
        }
        Ok(me)
    }

    fn put(&self, name: &str, v: f32) -> Result<()> {
        self.props.InsertScalar(&HSTRING::from(name), v)
    }

    fn put_pill(&self, s: Shape) -> Result<()> {
        for (k, v) in [("px", s.x), ("py", s.y), ("pw", s.w), ("ph", s.h), ("pr", s.r.max(0.5))] {
            self.put(k, v)?;
        }
        Ok(())
    }

    fn put_card(&self, s: Shape) -> Result<()> {
        for (k, v) in [("cx", s.x), ("cy", s.y), ("cw", s.w), ("ch", s.h), ("cr", s.r.max(0.5))] {
            self.put(k, v)?;
        }
        Ok(())
    }

    /// The pill is somewhere else now (or another size): the plate's far
    /// end, at once (unseen at p = 1).
    pub fn set_pill(&mut self, pill: Shape) {
        self.pill = pill;
        let _ = self.put_pill(pill);
    }

    /// The card's shape now, at once.
    pub fn set_card(&mut self, card: Shape) {
        let _ = self.props.StopAnimation(&HSTRING::from("ch"));
        let _ = self.put_card(card);
        self.card = card;
        self.height = SpringRun::still(SPRING_GLIDE, card.h, Instant::now());
    }

    /// What the plate shows now (drawn_at at `p` and the card's height
    /// where the springs have them; one that has arrived, exactly where it
    /// stands).
    pub fn drawn(&self) -> Drawn {
        let now = Instant::now();
        let p = if self.run.settled(now, SETTLED) { self.run.to } else { self.run.state(now).0 };
        let h = if self.height.settled(now, 0.25) { self.height.to } else { self.height.state(now).0 };
        drawn_at(self.pill, Shape { h, ..self.card }, p, self.up)
    }

    /// The card is `h` tall now: the plate's height glides there from
    /// where it is (7.4: a panel's height on GLIDE, its edge at the bar
    /// fixed). Returns how long it takes (ms).
    pub fn glide_height(&mut self, h: f32) -> u32 {
        self.card.h = h;
        match crate::motion::spring_prop(&self.c, &self.props, "ch", &self.height, h) {
            Ok(run) => self.height = run,
            Err(_) => {
                let _ = self.put("ch", h);
                self.height = SpringRun::still(SPRING_GLIDE, h, Instant::now());
            }
        }
        settle_ms(&self.height, Instant::now())
    }

    /// Sends `p` to `to` (1 the card, 0 the pill) on `spring`, from where
    /// it is at the speed it has. Returns how long it takes (ms; 0 with
    /// reduced motion: it is there at once).
    pub fn go(&mut self, to: f32, spring: Spring) -> u32 {
        let now = Instant::now();
        let (x, v) = self.run.state(now);
        let seed = SpringRun { spring, from: x, velocity: v, to: x, since: now, stretch: self.run.stretch };
        match crate::motion::spring_prop(&self.c, &self.props, "p", &seed, to) {
            Ok(run) => self.run = run,
            Err(_) => self.jump(to),
        }
        if self.run.settled(now, SETTLED) {
            0
        } else {
            settle_ms(&self.run, now)
        }
    }

    /// `p` is `to`, now, exactly (what a spring leaves a hair short of).
    pub fn jump(&mut self, to: f32) {
        let _ = self.props.StopAnimation(&HSTRING::from("p"));
        let _ = self.put("p", to);
        self.run = SpringRun::still(self.run.spring, to, Instant::now());
    }

    /// Where `p` is now, and where it is going.
    pub fn progress(&self) -> (f32, f32) {
        (self.run.state(Instant::now()).0, self.run.to)
    }

    /// `p` has arrived.
    pub fn settled(&self) -> bool {
        self.run.settled(Instant::now(), SETTLED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PILL: Shape = Shape { x: 47.0, y: 0.0, w: 246.0, h: 28.0, r: 14.0 };
    const CARD: Shape = Shape { x: 0.0, y: 0.0, w: 340.0, h: 204.0, r: 20.0 };

    #[test]
    fn one_progress_moves_the_whole_plate() {
        // The ends are the pill and the card, exactly.
        assert_eq!(plate_at(PILL, CARD, 0.0), PILL);
        assert_eq!(plate_at(PILL, CARD, 1.0), CARD);
        // Half way: half of everything (the mock-up's 246 + 94 p, 28 +
        // 176 p, 14 + 6 p).
        let half = plate_at(PILL, CARD, 0.5);
        assert_eq!((half.w, half.h, half.r), (293.0, 116.0, 17.0));
        // Its top stays on the pill's (the bar's edge), its middle too.
        for p in [0.0, 0.2, 0.35, 0.7, 1.0] {
            let s = plate_at(PILL, CARD, p);
            assert_eq!(s.y, 0.0, "{p}");
            assert!((s.x + s.w / 2.0 - 170.0).abs() < 1e-3, "{p}");
            // Never rounder than a capsule.
            assert!(s.r <= s.h / 2.0 + 1e-3, "{p}: {s:?}");
        }
        // A spring turned round may dip under 0: never smaller than the pill.
        assert_eq!(plate_at(PILL, CARD, -0.03), PILL);
    }

    #[test]
    fn the_contents_cross_at_the_split() {
        assert_eq!((pill_alpha(0.0), card_alpha(0.0)), (1.0, 0.0));
        assert_eq!((pill_alpha(SPLIT), card_alpha(SPLIT)), (0.0, 0.0), "the pill's is gone before the card's comes");
        assert_eq!((pill_alpha(1.0), card_alpha(1.0)), (0.0, 1.0));
        assert!((pill_alpha(0.175) - 0.5).abs() < 1e-6);
        assert!((card_alpha(0.675) - 0.5).abs() < 1e-6);
        // Backwards it is the same curve.
        for p in [0.1f32, 0.3, 0.5, 0.9] {
            assert!(pill_alpha(p) >= pill_alpha(p + 0.05));
            assert!(card_alpha(p) <= card_alpha(p + 0.05));
        }
        // Past the ends nothing goes out of range.
        assert_eq!((pill_alpha(-0.1), card_alpha(1.2), card_share(1.2), card_share(-0.2)), (1.0, 1.0, 1.0, 0.0));
    }

    #[test]
    fn each_content_stays_where_its_surface_had_it() {
        // Under a bar at the top: both hang from the plate's top middle.
        for p in [0.0, 0.35, 0.7, 1.0] {
            let s = plate_at(PILL, CARD, p);
            assert_eq!(pill_origin(s, PILL, false), (170.0, 0.0), "{p}");
            // The card's content: its middle on the plate's, at its top, as
            // wide as the plate at most (content_scale).
            let k = content_scale(s, CARD);
            let (cx, cy) = card_origin(s, CARD, false);
            assert!((cx + CARD.w * k / 2.0 - 170.0).abs() < 1e-3 && cy == 0.0, "{p}: {cx}, {cy}");
            assert!(cx >= s.x - 1e-3 && cx + CARD.w * k <= s.x + s.w + 1e-3, "{p}: never past the plate's sides");
        }
        assert_eq!(card_origin(plate_at(PILL, CARD, 1.0), CARD, false), (0.0, 0.0));
        // Over a bar at the bottom the pill is the card's bottom middle and
        // the plate grows up: both keep to its bottom.
        let low = Shape { y: CARD.h - PILL.h, ..PILL };
        for p in [0.0, 0.5, 1.0] {
            let s = plate_at(low, CARD, p);
            assert!((s.y + s.h - CARD.h).abs() < 1e-3, "the bottom edge holds: {s:?}");
            let (_, gy) = pill_origin(s, low, true);
            assert!((gy - low.y).abs() < 1e-3, "{p}");
            let (_, cy) = card_origin(s, CARD, true);
            assert!((cy + CARD.h * content_scale(s, CARD) - (s.y + s.h)).abs() < 1e-3, "{p}: its bottom on the plate's");
        }
        // A card the screen's edge pushed aside: its content slides in
        // with the plate and ends on the card's corner.
        let aside = Shape { x: -60.0, ..PILL };
        assert_eq!(card_origin(plate_at(aside, CARD, 1.0), CARD, false), (0.0, 0.0));
        assert!(card_origin(plate_at(aside, CARD, 0.0), CARD, false).0 < 0.0);
    }

    /// Final audit 3: mid-way the card's content (laid out for the whole
    /// card) ran past the narrower plate's sides. It is drawn as wide as
    /// the plate at most, growing to its own size with it; the pointer
    /// meets it through the same scale (Drawn::to_content).
    #[test]
    fn the_content_never_runs_past_the_plates_sides() {
        for up in [false, true] {
            for pill in [PILL, Shape { x: -60.0, ..PILL }] {
                let pill = if up { Shape { y: CARD.h - PILL.h, ..pill } } else { pill };
                for i in 0..=20 {
                    let p = i as f32 / 20.0;
                    let d = drawn_at(pill, CARD, p, up);
                    assert!(d.k > 0.0 && d.k <= 1.0);
                    assert!(d.origin.0 >= d.plate.x - 1e-3 && d.origin.0 + CARD.w * d.k <= d.plate.x + d.plate.w + 1e-3, "{p} {up}: {d:?}");
                    // A point of the content, shown and met again.
                    let (x, y) = (d.origin.0 + 40.0 * d.k, d.origin.1 + 30.0 * d.k);
                    let (cx, cy) = d.to_content(x, y);
                    assert!((cx - 40.0).abs() < 1e-3 && (cy - 30.0).abs() < 1e-3);
                }
                assert_eq!(drawn_at(pill, CARD, 1.0, up).k, 1.0);
            }
        }
        // The compositor scales it by the same expression.
        let src = include_str!("morph.rs");
        assert!(src.contains("bind(c, &me.content, \"Scale\""));
    }

    #[test]
    fn what_is_drawn_is_one_shape_and_one_place() {
        // The plate and the content's place at p, as the expressions have
        // them (the card's input meets the same).
        for up in [false, true] {
            let pill = if up { Shape { y: CARD.h - PILL.h, ..PILL } } else { PILL };
            for p in [0.0, 0.35, 0.7, 1.0] {
                let d = drawn_at(pill, CARD, p, up);
                assert_eq!(d.plate, plate_at(pill, CARD, p));
                assert_eq!(d.origin, card_origin(d.plate, CARD, up));
                assert_eq!((d.p, d.pill), (p, pill));
            }
        }
        // On the shape: inside its box, not in a cut corner.
        let s = Shape { x: 10.0, y: 0.0, w: 100.0, h: 40.0, r: 20.0 };
        assert!(on_shape(s, 60.0, 20.0));
        assert!(on_shape(s, 30.0, 1.0), "the top edge past the corner");
        assert!(!on_shape(s, 11.0, 1.0), "the corner is cut");
        assert!(on_shape(s, 10.0 + 20.0 - 14.0, 20.0 - 14.0), "inside the corner's arc");
        assert!(!on_shape(s, 9.0, 20.0) && !on_shape(s, 110.0, 20.0) && !on_shape(s, 60.0, 40.0));
        // A radius past the half height is a capsule's.
        assert!(on_shape(Shape { r: 99.0, ..s }, 60.0, 0.5));
    }

    #[test]
    fn the_morph_turns_round_where_it_is() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        // Opening: GLIDE from the pill.
        let open = SpringRun::still(SPRING_GLIDE, 0.0, t0).toward(1.0, t0, 1.0);
        let whole = settle_ms(&open, t0);
        assert!((200..=700).contains(&whole), "{whole}");
        // Pressed again 120 ms in: back from where it is, at the speed it
        // has (it still moves on for a moment: nothing starts over).
        let (x, v) = open.state(ms(120));
        assert!(x > 0.2 && x < 0.9 && v > 0.0, "{x} {v}");
        let back = open.toward(0.0, ms(120), 1.0);
        assert_eq!(back.state(ms(120)), (x, v));
        assert!(back.state(ms(130)).0 > x - 0.05);
        assert!(back.state(ms(600)).0.abs() < 0.01);
        // From part of the way it is back sooner than a whole way.
        assert!(settle_ms(&back, ms(120)) <= whole + 80);
        // And opened once more while it closes.
        let again = back.toward(1.0, ms(200), 1.0);
        assert!((again.state(ms(900)).0 - 1.0).abs() < 0.01);
        // Arrived: nothing left to wait for.
        assert_eq!(settle_ms(&SpringRun::still(SPRING_GLIDE, 1.0, t0), t0), 8);
    }

    #[test]
    fn the_shadow_patch_scales_to_the_plates_radius() {
        // At its own radius on a 150 % display its insets are its pixels.
        assert!((inset_scale(1.0, 1.5) - 1.0 / 1.5).abs() < 1e-6);
        // The card's patch (corners 20) as the pill's shadow (14).
        assert!((inset_scale(14.0 / 20.0, 1.0) - 0.7).abs() < 1e-6);
        // The contact ring sits just outside the plate.
        let ((ox, oy), (w, h), r, k) = contact_ring(100.0, 28.0, 14.0);
        assert_eq!(k, crate::theme::CONTACT_W);
        assert_eq!((ox, oy), (1.0 - k / 2.0, 1.0 - k / 2.0));
        assert_eq!((w, h, r), (100.0 + k, 28.0 + k, 14.0 + k / 2.0));
    }

    #[test]
    fn the_plates_look_is_the_tokens() {
        use crate::syslook::SysLook;
        for light in [false, true] {
            let t = Theme::graphite().resolve(&SysLook { light, ..Default::default() });
            let l = Look::with(&t, true, true, 1.0);
            // The pill is M4, the card M2: a step apart, the pill lighter.
            assert_eq!(l.pill, t.material(Material::Tip));
            assert_eq!((l.card.r, l.card.g, l.card.b), { let c = t.material(Material::Panel); (c.r, c.g, c.b) });
            assert!(l.pill.r > l.card.r, "{light}");
            assert_eq!((l.e1.len(), l.e3.len()), (2, 2), "two layers each");
            assert!(l.rim.is_some() && l.outline.is_none() && l.contact.a > 0);
            // The "Shadows" effect off: none, the rim and contact stay.
            let off = Look::with(&t, false, true, 1.0);
            assert!(off.e1.is_empty() && off.e3.is_empty() && off.rim.is_some());
            // High contrast: Window, an outline, no light at all.
            let mut hc = t.clone();
            hc.high_contrast = true;
            let (white, black) = (Rgba { r: 255, g: 255, b: 255, a: 255 }, Rgba { r: 0, g: 0, b: 0, a: 255 });
            hc.hc = Some(crate::syslook::HighContrast { window: black, text: white, highlight: white, highlight_text: black, gray_text: crate::syslook::GRAY_TEXT });
            let l = Look::with(&hc, true, true, 1.0);
            assert!(l.rim.is_none() && l.outline.is_some() && l.contact.a == 0 && l.e1.is_empty() && l.e3.is_empty());
        }
    }

    #[test]
    fn the_plates_ends_wear_the_pill_and_the_panel() {
        use crate::syslook::SysLook;
        use crate::theme::Finish;
        for light in [false, true] {
            let sys = SysLook { light, ..Default::default() };
            // Flat v2 (the Flat one chose): the pill at rest is the plate at
            // p = 0, a panel's plate is the plate at p = 1: the same falls,
            // rims (1 DIP at every scale) and grain.
            let t = Theme::graphite().resolve_fx(&sys, true);
            for s in [1.0, 1.5, 2.0] {
                let l = Look::with(&t, true, true, s);
                let (pill, card) = l.ends();
                assert_eq!(pill, super::super::pill_coat(&t, s), "light {light} {s}: the pill at rest is p = 0");
                assert_eq!(card, t.plate_coat(Material::Panel, l.card, s), "light {light} {s}: a panel is p = 1");
                // Flat (since 2026-11-12): one grey each, no rim, no grain.
                assert!(pill.fill.0 == pill.fill.1 && card.fill.0 == card.fill.1, "one grey each");
                assert_eq!((pill.rim.map(|r| r.0.map(|c| c.a)), card.rim.map(|r| r.0.map(|c| c.a))), (Some([0; 3]), Some([0; 3])), "no rim");
                assert_eq!((pill.grain, card.grain), (0.0, 0.0), "no grain");
            }
            // A Flat the PC forces (transparency off, the blur off): the
            // same plates, no grain at either end.
            for (sys, blur) in [(SysLook { light, transparency: false, ..Default::default() }, true), (sys, false)] {
                let f = Theme::graphite().resolve_fx(&sys, blur);
                let (pill, card) = Look::with(&f, true, blur, 1.5).ends();
                assert_eq!(pill, super::super::pill_coat(&f, 1.5));
                assert_eq!((pill.grain, card.grain), (0.0, 0.0));
                assert!(card.rim.is_some() && pill.rim.is_some());
            }
            // A glass keeps its own: one color each, one rim at both ends
            // (the glass's light, one device pixel: final sheet 2), no top
            // light or grain on the morphing shape.
            let mut raw = Theme::graphite();
            raw.finish = Finish::Glass;
            let g = raw.resolve_fx(&sys, true);
            let l = Look::with(&g, true, true, 1.5);
            let (pill, card) = l.ends();
            assert_eq!(pill, super::super::pill_coat(&g, 1.5));
            assert!(pill.fill.0 == pill.fill.1 && card.fill.0 == card.fill.1);
            assert_eq!(pill.rim, card.rim);
            assert!(l.rim_carries());
            assert_eq!(card.rim.map(|r| r.1), Some(1.0 / 1.5));
            assert_eq!(card.grain, 0.0);
        }
    }
}
