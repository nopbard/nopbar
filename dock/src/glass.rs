//! Glass v3's plates on screen (design system 2.1; the numbers and the
//! decisions: docs/design/harmony/glass-v3/results.md).
//!
//! A glass plate, bottom up: DWM's blurred backdrop (HostBackdropBrush)
//! through one Composition effect graph, Saturation and then the
//! Luminosity layer (a Blend of the material's grey that sets the
//! backdrop's lightness and keeps its hue and saturation: Windows
//! Acrylic's 19H1 recipe, made achromatic); the tint over it (a plain
//! color visual, as before); the top light and the grain (top_light);
//! the rim (popup::rim_visual, the dock's highlight). Everything we paint
//! is achromatic: the color is the wallpaper's alone.
//!
//! Win2D is not there in Rust, so the effect descriptions are our own COM
//! objects (IGraphicsEffect + IGraphicsEffectSource +
//! IGraphicsEffectD2D1Interop, D2D's CLSIDs and property indices). The
//! graph is compiled once per compositor (a CompositionEffectFactory, kept
//! for the thread) and every plate takes a brush of it, its saturation and
//! Luminosity layer set as the factory's animatable properties.
//!
//! Falling back (one log line each): a brush is made at the best Level
//! that works all the way (its factory loaded, the brush made, its source
//! set, its properties set): the full graph, else saturation alone, else
//! the plain HostBackdropBrush. The brush carries its level (its Comment),
//! and the tint over it is chosen by that level (tint_for): where the
//! Luminosity layer is not drawn the tint takes its share (fold: the same
//! lightness, so the same contrast). A plate's look never depends on a
//! factory having loaded, only on the brush it really has.

use std::cell::RefCell;
use std::mem::ManuallyDrop;
use std::sync::Mutex;

use windows::core::{implement, Error, IInspectable, Interface, Result, GUID, HSTRING, PCWSTR};
use windows::Foundation::{IPropertyValue, PropertyValue};
use windows::Graphics::Effects::{IGraphicsEffect, IGraphicsEffectSource, IGraphicsEffectSource_Impl, IGraphicsEffect_Impl};
use windows::UI::Color;
use windows::UI::Composition::{
    CompositionBackdropBrush, CompositionBrush, CompositionEffectBrush, CompositionEffectFactory, CompositionEffectFactoryLoadStatus, CompositionEffectSourceParameter, CompositionStretch, CompositionSurfaceBrush, Compositor, ContainerVisual, SpriteVisual, Visual,
};
use windows::Win32::Foundation::{E_BOUNDS, E_INVALIDARG, E_UNEXPECTED};
use windows::Win32::Graphics::Direct2D::{CLSID_D2D1Blend, CLSID_D2D1Border, CLSID_D2D1Flood, CLSID_D2D1Saturation};
use windows::Win32::System::WinRT::Graphics::Direct2D::{
    IGraphicsEffectD2D1Interop, IGraphicsEffectD2D1Interop_Impl, GRAPHICS_EFFECT_PROPERTY_MAPPING, GRAPHICS_EFFECT_PROPERTY_MAPPING_COLOR_TO_VECTOR4, GRAPHICS_EFFECT_PROPERTY_MAPPING_DIRECT,
};
use windows_numerics::Vector2;

use crate::gfx::Gfx;
use crate::theme::{GlassFx, Material, Rgba, Theme};

/// The plate graph's source parameter (the HostBackdropBrush).
const BACKDROP: &str = "Backdrop";
/// The grain graph's source parameter (the noise tile).
const NOISE: &str = "Noise";
/// The animatable properties: "<effect name>.<property name>".
const SAT: &str = "Sat.Saturation";
const LUM: &str = "LumColor.Color";

/// The Blend mode that gives the Luminosity layer (the backdrop's hue and
/// saturation, the grey's lightness): D2D1_BLEND_MODE_COLOR (22), not
/// LUMINOSITY (23). The names are flipped on the compositor, as WinUI 2's
/// Acrylic notes (it asks for Color to get luminosity): read back off
/// screen on 26200 (glass_graph_on_the_compositor), 22 matches the
/// luminosity formulas to the pixel and 23 gives the color blend (the
/// grey's lack of hue at the backdrop's lightness: a grey wash).
const LUM_MODE: u32 = 22;
/// D2D1_BORDER_EDGE_MODE_WRAP: the grain tile repeats.
const WRAP: u32 = 1;
/// The grain tile's side (px): small, made once per compositor, tiled.
pub const TILE: u32 = 128;

/// One D2D effect property's value.
#[derive(Clone, Copy)]
enum Prop {
    F32(f32),
    U32(u32),
    Vec4([f32; 4]),
}

/// One effect in the graph: D2D's CLSID, its properties by index, its
/// inputs, and the properties that may be set on a brush by name.
///
/// The object is agile (windows-implement's default: it answers
/// IAgileObject and hands out the free-threaded marshaler), so any thread
/// may call it at any time: everything but the name is fixed at birth and
/// the name is behind a lock (a poisoned lock is E_UNEXPECTED, not a
/// panic across COM).
#[implement(IGraphicsEffect, IGraphicsEffectSource, IGraphicsEffectD2D1Interop)]
struct Effect {
    id: GUID,
    name: Mutex<HSTRING>,
    props: Vec<Prop>,
    sources: Vec<IGraphicsEffectSource>,
    named: Vec<(&'static str, u32, GRAPHICS_EFFECT_PROPERTY_MAPPING)>,
}

impl IGraphicsEffect_Impl for Effect_Impl {
    fn Name(&self) -> Result<HSTRING> {
        Ok(self.name.lock().map_err(|_| Error::from(E_UNEXPECTED))?.clone())
    }

    fn SetName(&self, name: &HSTRING) -> Result<()> {
        *self.name.lock().map_err(|_| Error::from(E_UNEXPECTED))? = name.clone();
        Ok(())
    }
}

impl IGraphicsEffectSource_Impl for Effect_Impl {}

impl IGraphicsEffectD2D1Interop_Impl for Effect_Impl {
    fn GetEffectId(&self) -> Result<GUID> {
        Ok(self.id)
    }

    fn GetNamedPropertyMapping(&self, name: &PCWSTR, index: *mut u32, mapping: *mut GRAPHICS_EFFECT_PROPERTY_MAPPING) -> Result<()> {
        if name.is_null() {
            return Err(Error::from(E_INVALIDARG));
        }
        let n = unsafe { name.to_string() }.unwrap_or_default();
        let &(_, i, m) = self.named.iter().find(|(k, ..)| k.eq_ignore_ascii_case(&n)).ok_or_else(|| Error::from(E_INVALIDARG))?;
        unsafe {
            if !index.is_null() {
                *index = i;
            }
            if !mapping.is_null() {
                *mapping = m;
            }
        }
        Ok(())
    }

    fn GetPropertyCount(&self) -> Result<u32> {
        Ok(self.props.len() as u32)
    }

    fn GetProperty(&self, index: u32) -> Result<IPropertyValue> {
        let v: IInspectable = match self.props.get(index as usize).ok_or_else(|| Error::from(E_BOUNDS))? {
            Prop::F32(f) => PropertyValue::CreateSingle(*f)?,
            Prop::U32(u) => PropertyValue::CreateUInt32(*u)?,
            Prop::Vec4(v) => PropertyValue::CreateSingleArray(v)?,
        };
        v.cast()
    }

    fn GetSource(&self, index: u32) -> Result<IGraphicsEffectSource> {
        self.sources.get(index as usize).cloned().ok_or_else(|| Error::from(E_BOUNDS))
    }

    fn GetSourceCount(&self) -> Result<u32> {
        Ok(self.sources.len() as u32)
    }
}

fn effect(id: GUID, name: &str, props: Vec<Prop>, sources: Vec<IGraphicsEffectSource>, named: Vec<(&'static str, u32, GRAPHICS_EFFECT_PROPERTY_MAPPING)>) -> IGraphicsEffect {
    Effect { id, name: Mutex::new(HSTRING::from(name)), props, sources, named }.into()
}

fn param(name: &str) -> Result<IGraphicsEffectSource> {
    CompositionEffectSourceParameter::Create(&HSTRING::from(name))?.cast()
}

/// The saturation effect over `source` (its saturation animatable).
fn sat_effect(source: IGraphicsEffectSource) -> IGraphicsEffect {
    effect(CLSID_D2D1Saturation, "Sat", vec![Prop::F32(1.0)], vec![source], vec![("Saturation", 0, GRAPHICS_EFFECT_PROPERTY_MAPPING_DIRECT)])
}

/// The Luminosity layer's grey (its color animatable).
fn lum_flood() -> IGraphicsEffect {
    effect(CLSID_D2D1Flood, "LumColor", vec![Prop::Vec4([0.0; 4])], Vec::new(), vec![("Color", 0, GRAPHICS_EFFECT_PROPERTY_MAPPING_COLOR_TO_VECTOR4)])
}

/// The plate's graph: Backdrop → Saturation → (Blend: the Luminosity
/// layer, a Flood of the grey over it). `blend`: the Blend's mode (None:
/// saturation alone).
fn plate_graph(blend: Option<u32>) -> Result<IGraphicsEffect> {
    let sat = sat_effect(param(BACKDROP)?);
    let Some(mode) = blend else { return Ok(sat) };
    // Input 0 is the background (D2D's destination), 1 the foreground.
    Ok(effect(CLSID_D2D1Blend, "Lum", vec![Prop::U32(mode)], vec![sat.cast()?, lum_flood().cast()?], Vec::new()))
}

/// Pure: a factory's load status is one a brush may be made of (Pending:
/// still loading, the compositor queues the brush).
fn usable(s: CompositionEffectFactoryLoadStatus) -> bool {
    matches!(s, CompositionEffectFactoryLoadStatus::Success | CompositionEffectFactoryLoadStatus::Pending)
}

/// `f`'s load status as an error unless usable.
fn loaded(f: &CompositionEffectFactory) -> Result<()> {
    let s = f.LoadStatus()?;
    if usable(s) {
        Ok(())
    } else {
        Err(Error::new(f.ExtendedError().unwrap_or(E_INVALIDARG), format!("load status {}", s.0)))
    }
}

/// `graph` compiled with `names` animatable; an error unless it loaded or
/// is loading.
fn compile(c: &Compositor, graph: &IGraphicsEffect, names: &[&str]) -> Result<CompositionEffectFactory> {
    let names: Vec<HSTRING> = names.iter().map(|n| HSTRING::from(*n)).collect();
    let f = c.CreateEffectFactoryWithProperties(graph, &windows_collections::IIterable::<HSTRING>::from(names))?;
    loaded(&f)?;
    Ok(f)
}

/// The plate's factory with Blend mode `blend` (None: saturation alone).
fn plate_factory(c: &Compositor, blend: Option<u32>) -> Result<CompositionEffectFactory> {
    let graph = plate_graph(blend)?;
    if blend.is_some() {
        compile(c, &graph, &[SAT, LUM])
    } else {
        compile(c, &graph, &[SAT])
    }
}

/// How much of a glass's graph a plate's brush draws (best last).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    /// The plain HostBackdropBrush (or any brush not made here).
    Plain,
    /// Saturation alone.
    Sat,
    /// Saturation and the Luminosity layer.
    Full,
}

impl Level {
    /// The levels a brush is tried at, best first (Plain always works:
    /// it is the host backdrop as it is).
    pub const LADDER: [Level; 2] = [Level::Full, Level::Sat];

    /// The mark a brush of this level carries (its Comment).
    fn tag(self) -> &'static str {
        match self {
            Level::Full => "glass-full",
            Level::Sat => "glass-sat",
            Level::Plain => "",
        }
    }

    /// Pure: the level a brush marked `tag` draws (Plain: no mark of ours).
    fn of_tag(tag: &str) -> Level {
        [Level::Full, Level::Sat].into_iter().find(|l| l.tag() == tag).unwrap_or(Level::Plain)
    }

    /// Pure: what of glass `fx` this level draws (the rest: none: the
    /// Luminosity layer's alpha 0, saturation 1).
    pub fn fx(self, fx: GlassFx) -> GlassFx {
        match self {
            Level::Full => fx,
            Level::Sat => GlassFx { lum: Rgba { a: 0, ..fx.lum }, ..fx },
            Level::Plain => GlassFx { sat: 1.0, lum: Rgba { a: 0, ..fx.lum }, ..fx },
        }
    }
}

/// One factory of the kit: not compiled yet, compiled (loaded or
/// loading), or failed (for good on this compositor).
#[derive(Clone, Default)]
enum Slot {
    #[default]
    Untried,
    Ready(CompositionEffectFactory),
    Failed,
}

/// The grain's factory and its tile, or None when they could not be made;
/// `generation`: the rendering device's (Gfx::generation) they were made
/// on: the tile's pixels are lost with it.
struct Grain {
    generation: u32,
    made: Option<(CompositionEffectFactory, CompositionSurfaceBrush)>,
}

/// What grain() does with the kit's grain: use it, make it (none yet, or
/// it failed on another device), or upload the tile's pixels again (the
/// device was swapped under it: same surface brush, a new surface).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GrainStep {
    Use,
    Make,
    Refill,
}

/// Pure: the grain's step for what is cached (`cached`: its device
/// generation and whether it was made) on device generation `now`.
fn grain_step(cached: Option<(u32, bool)>, now: u32) -> GrainStep {
    match cached {
        None => GrainStep::Make,
        Some((g, made)) if g != now => {
            if made {
                GrainStep::Refill
            } else {
                GrainStep::Make
            }
        }
        // This device: as it is (a failure is not tried again on it).
        Some(_) => GrainStep::Use,
    }
}

/// What one compositor's plates share (made once per thread).
struct Kit {
    compositor: Compositor,
    /// The plate's factories: the full graph and saturation alone.
    full: Slot,
    sat: Slot,
    grain: Option<Grain>,
}

impl Kit {
    fn new(c: &Compositor) -> Kit {
        Kit { compositor: c.clone(), full: Slot::Untried, sat: Slot::Untried, grain: None }
    }

    /// The plate's factory for `level` (compiled on first use), None when
    /// it cannot be had (failed to compile or to load: logged once).
    fn factory(&mut self, level: Level) -> Option<CompositionEffectFactory> {
        let (slot, blend) = match level {
            Level::Full => (&mut self.full, Some(LUM_MODE)),
            Level::Sat => (&mut self.sat, None),
            Level::Plain => return None,
        };
        if matches!(slot, Slot::Untried) {
            *slot = match plate_factory(&self.compositor, blend) {
                Ok(f) => Slot::Ready(f),
                Err(e) => {
                    crate::log!("glass: no {level:?} graph ({e})");
                    Slot::Failed
                }
            };
        }
        // A factory that was still loading may have failed since.
        if let Slot::Ready(f) = slot {
            if let Err(e) = loaded(f) {
                crate::log!("glass: the {level:?} graph did not load ({e})");
                *slot = Slot::Failed;
            }
        }
        match slot {
            Slot::Ready(f) => Some(f.clone()),
            _ => None,
        }
    }
}

thread_local! {
    /// This thread's kit. Never dropped at the thread's end (COM may be
    /// gone by then): replaced only when the compositor is another.
    static KIT: RefCell<Option<ManuallyDrop<Kit>>> = const { RefCell::new(None) };
}

fn with_kit<R>(c: &Compositor, f: impl FnOnce(&mut Kit) -> R) -> R {
    KIT.with(|k| {
        let mut k = k.borrow_mut();
        if k.as_ref().is_none_or(|kit| kit.compositor != *c) {
            if let Some(mut old) = k.take() {
                unsafe { ManuallyDrop::drop(&mut old) };
            }
            *k = Some(ManuallyDrop::new(Kit::new(c)));
        }
        f(k.as_mut().expect("made above"))
    })
}

/// Pure: the first of Level::LADDER that `make` makes (None from it: that
/// level cannot be had, nothing to log; an error: logged, the next level
/// tried), else None: the plain backdrop.
fn ladder<B>(mut make: impl FnMut(Level) -> Option<Result<B>>) -> Option<(Level, B)> {
    for level in Level::LADDER {
        match make(level) {
            Some(Ok(b)) => return Some((level, b)),
            Some(Err(e)) => crate::log!("glass: no {level:?} plate ({e}); one level down"),
            None => {}
        }
    }
    None
}

/// Pure: `tint` with a Luminosity layer `lum` folded in (its opacity
/// added the way two coats of one grey add up): over a grey backdrop the
/// same lightness as the layer and the tint, so the same text contrast;
/// over color, less of it.
pub fn fold(tint: Rgba, lum: Rgba) -> Rgba {
    let (a, l) = (tint.a as f32 / 255.0, lum.a as f32 / 255.0);
    Rgba { a: ((1.0 - (1.0 - a) * (1.0 - l)) * 255.0).round() as u8, ..tint }
}

/// Pure: the tint to lay over material `m`'s backdrop drawn at `level`:
/// Theme::material's, the Luminosity layer folded in where the brush
/// does not draw it.
pub fn tint_at(theme: &Theme, m: Material, level: Level) -> Rgba {
    let t = theme.material(m);
    match theme.glass_fx(m) {
        Some(fx) if fx.lum.a > 0 && level != Level::Full => fold(t, fx.lum),
        _ => t,
    }
}

/// The level `brush` draws (the mark backdrop() put on it; Plain for any
/// other brush).
pub fn level_of(brush: &CompositionBrush) -> Level {
    brush.Comment().map_or(Level::Plain, |t| Level::of_tag(&t.to_string()))
}

/// The tint to lay over material `m`'s backdrop `brush` (the one the
/// plate really has; None: none, as Plain).
pub fn tint_for(theme: &Theme, m: Material, brush: Option<&CompositionBrush>) -> Rgba {
    tint_at(theme, m, brush.map_or(Level::Plain, level_of))
}

/// The brush for material `m`'s backdrop: DWM's blur through the glass's
/// graph at the best level that works (a glass in force), else the plain
/// HostBackdropBrush (Flat shows none; no level could be made). Its tint:
/// tint_for() with it. An error only when the host backdrop itself cannot
/// be made.
pub fn backdrop(gfx: &Gfx, theme: &Theme, m: Material) -> Result<CompositionBrush> {
    let c = &gfx.compositor;
    let host = host_backdrop(c)?;
    Ok(match theme.glass_fx(m) {
        Some(fx) => over(c, host, fx.sat, fx.lum),
        None => host,
    })
}

/// The brush for the plate that covers the screen (Launchpad's scrim:
/// Theme::scrim over it): the panels' saturation, no Luminosity layer
/// (the scrim is its own lightness).
pub fn scrim_backdrop(gfx: &Gfx, theme: &Theme) -> Result<CompositionBrush> {
    let c = &gfx.compositor;
    let host = host_backdrop(c)?;
    Ok(match theme.glass_fx(Material::Panel) {
        Some(fx) => over(c, host, fx.sat, Rgba { a: 0, ..fx.lum }),
        None => host,
    })
}

/// DWM's blur behind the window (HostBackdropBrush), what every glass
/// graph reads. Off screen it draws nothing (a render test reads its own
/// visuals back), so a render test puts an opaque stand-in there
/// (`StandIn`): the picture's one-colored backdrop, which DWM's blur
/// would leave as it is. Then the shadows under a plate stay hidden as
/// they do on screen, and the graph works on what it would see.
pub(crate) fn host_backdrop(c: &Compositor) -> Result<CompositionBrush> {
    #[cfg(test)]
    if let Some(g) = STAND_IN.with(|s| s.get()) {
        return c.CreateColorBrushWithColor(windows::UI::Color { A: 255, R: g.r, G: g.g, B: g.b })?.cast();
    }
    c.CreateHostBackdropBrush()?.cast()
}

#[cfg(test)]
thread_local! {
    static STAND_IN: std::cell::Cell<Option<Rgba>> = const { std::cell::Cell::new(None) };
}

/// While it lives, this thread's backdrop brushes are `ground` (tests
/// only: host_backdrop).
#[cfg(test)]
pub(crate) struct StandIn(Option<Rgba>);

#[cfg(test)]
impl StandIn {
    pub(crate) fn new(ground: Rgba) -> StandIn {
        StandIn(STAND_IN.with(|s| s.replace(Some(Rgba { a: 255, ..ground }))))
    }
}

#[cfg(test)]
impl Drop for StandIn {
    fn drop(&mut self) {
        STAND_IN.with(|s| s.set(self.0));
    }
}

/// A step of making a plate's brush (where a test may inject a fault).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Factory,
    Create,
    Source,
    Tune,
}

#[cfg(test)]
thread_local! {
    /// The faults a test injected: (level, step) fails.
    pub(crate) static FAULTS: RefCell<Vec<(Level, Step)>> = const { RefCell::new(Vec::new()) };
}

/// Fails when a test injected a fault at (`level`, `step`); nothing in the
/// app.
fn fault(level: Level, step: Step) -> Result<()> {
    #[cfg(test)]
    if FAULTS.with(|f| f.borrow().contains(&(level, step))) {
        return Err(Error::new(E_UNEXPECTED, format!("injected at {step:?}")));
    }
    let _ = (level, step);
    Ok(())
}

/// A brush of `f` at `level` over `source`, set and marked; an error at
/// any step (the caller goes one level down).
fn make_brush(f: &CompositionEffectFactory, level: Level, source: &CompositionBrush, sat: f32, lum: Rgba) -> Result<CompositionBrush> {
    fault(level, Step::Create)?;
    let b = f.CreateBrush()?;
    fault(level, Step::Source)?;
    b.SetSourceParameter(&HSTRING::from(BACKDROP), source)?;
    fault(level, Step::Tune)?;
    tune(&b, level, sat, lum)?;
    b.SetComment(&HSTRING::from(level.tag()))?;
    b.cast()
}

/// `source` through the plate's graph at the best level that can be made
/// (as it is when none can).
pub fn over(c: &Compositor, source: CompositionBrush, sat: f32, lum: Rgba) -> CompositionBrush {
    let made = ladder(|level| {
        if fault(level, Step::Factory).is_err() {
            return None;
        }
        let f = with_kit(c, |k| k.factory(level))?;
        Some(make_brush(&f, level, &source, sat, lum))
    });
    made.map_or(source, |(_, b)| b)
}

/// Sets brush `b` of `level`'s properties: its saturation, and the
/// Luminosity layer's grey where the level draws it.
fn tune(b: &CompositionEffectBrush, level: Level, sat: f32, lum: Rgba) -> Result<()> {
    let p = b.Properties()?;
    p.InsertScalar(&HSTRING::from(SAT), sat)?;
    if level == Level::Full {
        p.InsertColor(&HSTRING::from(LUM), Color { A: lum.a, R: lum.r, G: lum.g, B: lum.b })?;
    }
    Ok(())
}

/// A brush backdrop() made, set for `theme`'s material `m` (another
/// glass, the other palette) without a new brush (a new one makes DWM
/// set the backdrop up again), at the level it was made at. false: not
/// one of ours, no glass now, or it could not be set.
pub fn retune(brush: &CompositionBrush, theme: &Theme, m: Material) -> bool {
    let level = level_of(brush);
    let (Ok(b), Some(fx), true) = (brush.cast::<CompositionEffectBrush>(), theme.glass_fx(m), level != Level::Plain) else { return false };
    tune(&b, level, fx.sat, fx.lum).is_ok()
}

/// Some level of the plate's graph can be had on `c` (false: only the
/// plain backdrop).
pub fn has_graph(c: &Compositor) -> bool {
    with_kit(c, |k| Level::LADDER.iter().any(|&l| k.factory(l).is_some()))
}

/// `v`'s backdrop for `theme`'s material `m`, kept where it can be: the
/// brush it has retuned when it is ours, a plain host backdrop kept while
/// no glass asks for more (or no graph can be made), a new brush
/// otherwise (a new brush makes DWM set the backdrop up again). Returns
/// the tint for the brush `v` has now (tint_for): lay it over.
pub fn refresh_backdrop(gfx: &Gfx, v: &SpriteVisual, theme: &Theme, m: Material) -> Rgba {
    let c = &gfx.compositor;
    let cur = v.Brush().ok();
    let keep = match &cur {
        Some(b) if retune(b, theme, m) => true,
        Some(b) => b.cast::<CompositionBackdropBrush>().is_ok() && (theme.glass_fx(m).is_none() || !has_graph(c)),
        None => false,
    };
    if !keep {
        match backdrop(gfx, theme, m) {
            Ok(b) => {
                if let Err(e) = v.SetBrush(&b) {
                    crate::log!("glass: backdrop not set ({e})");
                }
            }
            Err(e) => crate::log!("glass: no backdrop ({e})"),
        }
    }
    tint_for(theme, m, v.Brush().ok().as_ref())
}

/// `v`'s backdrop for a plate that keeps one saturation `sat` and no
/// Luminosity layer whatever the theme's glass (the top bar's Frost:
/// topbar::look::FROST_SAT), kept where it can be as refresh_backdrop
/// keeps it: the brush it has retuned when it is ours, the plain host
/// backdrop kept while no graph can be made, a new brush otherwise. Its
/// tint is the caller's own (no Luminosity layer to fold in).
pub fn refresh_fixed(gfx: &Gfx, v: &SpriteVisual, sat: f32) {
    let c = &gfx.compositor;
    let none = Rgba { r: 0, g: 0, b: 0, a: 0 };
    let cur = v.Brush().ok();
    let keep = match &cur {
        Some(b) if level_of(b) != Level::Plain => b.cast::<CompositionEffectBrush>().is_ok_and(|e| tune(&e, level_of(b), sat, none).is_ok()),
        Some(b) => b.cast::<CompositionBackdropBrush>().is_ok() && !has_graph(c),
        None => false,
    };
    if keep {
        return;
    }
    match host_backdrop(c) {
        Ok(host) => {
            if let Err(e) = v.SetBrush(&over(c, host, sat, none)) {
                crate::log!("glass: backdrop not set ({e})");
            }
        }
        Err(e) => crate::log!("glass: no backdrop ({e})"),
    }
}

/// The brush of material `m`'s tint: Flat v2's fall from top to bottom
/// (Theme::flat_fill, relative to the visual it fills), else `tint` as one
/// color.
pub fn tint_brush(c: &Compositor, theme: &Theme, m: Material, tint: Rgba) -> Result<CompositionBrush> {
    fall_brush(c, theme.flat_fill(m), tint)
}

/// A brush of `fall` (top, bottom: a fall down the visual it fills,
/// relatively), else `tint` as one color.
pub fn fall_brush(c: &Compositor, fall: Option<(Rgba, Rgba)>, tint: Rgba) -> Result<CompositionBrush> {
    let col = |t: Rgba| Color { A: t.a, R: t.r, G: t.g, B: t.b };
    match fall {
        Some((top, bottom)) => {
            let lin = c.CreateLinearGradientBrush()?;
            lin.SetStartPoint(Vector2 { X: 0.0, Y: 0.0 })?;
            lin.SetEndPoint(Vector2 { X: 0.0, Y: 1.0 })?;
            lin.ColorStops()?.Append(&c.CreateColorGradientStopWithOffsetAndColor(0.0, col(top))?)?;
            lin.ColorStops()?.Append(&c.CreateColorGradientStopWithOffsetAndColor(1.0, col(bottom))?)?;
            lin.cast()
        }
        None => c.CreateColorBrushWithColor(col(tint))?.cast(),
    }
}

/// Marks the visual top_light made (place_top_light finds it again).
const TAG_LIGHT: &str = "glass-top-light";

/// Material `m`'s top light and grain in `parent`, just over `under` (the
/// tint), at the parent's size; the one placed before (by this) is taken
/// out first. Nothing when `shown` is false (the backdrop is not seen) or
/// the glass has none (Flat, high contrast).
pub fn place_top_light(gfx: &Gfx, theme: &Theme, m: Material, parent: &ContainerVisual, under: &Visual, shown: bool) {
    let Ok(kids) = parent.Children() else { return };
    let old: Vec<Visual> = kids.clone().into_iter().filter(|k| k.Comment().is_ok_and(|t| t == TAG_LIGHT)).collect();
    for k in old {
        let _ = kids.Remove(&k);
    }
    if !shown {
        return;
    }
    match top_light(gfx, theme, m, None) {
        Ok(Some(l)) => {
            let _ = l.SetComment(&HSTRING::from(TAG_LIGHT));
            let _ = kids.InsertAbove(&l, under);
        }
        Ok(None) => {}
        Err(e) => crate::log!("glass: no top light ({e})"),
    }
}

/// Pure: the grain tile: grey at half lightness, its alpha even noise
/// (premultiplied BGRA, TILE x TILE; the same every time).
pub fn noise_tile() -> Vec<u8> {
    let mut x: u32 = 0x9E37_79B9;
    let mut out = Vec::with_capacity((TILE * TILE * 4) as usize);
    for _ in 0..TILE * TILE {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        let a = (x >> 24) as u8;
        let v = (a as u32 * 128 / 255) as u8;
        out.extend_from_slice(&[v, v, v, a]);
    }
    out
}

/// A brush of the grain, tiled (None: it could not be made). The tile is
/// made once per compositor and filled again when the rendering device
/// was swapped under it (Gfx::generation: a recovery keeps the surfaces
/// but not their pixels): the same surface brush gets a new surface, so
/// the grain already placed shows again too. A failure is tried again on
/// the next device.
fn grain(gfx: &Gfx) -> Option<CompositionBrush> {
    let c = &gfx.compositor;
    let now = gfx.generation();
    let tile_surface = || gfx.surface_from_bgra(TILE, TILE, &noise_tile());
    let made = with_kit(c, |k| {
        let cached = k.grain.as_ref().map(|g| (g.generation, g.made.is_some()));
        match grain_step(cached, now) {
            GrainStep::Use => {}
            GrainStep::Make => {
                let made = (|| -> Result<_> {
                    let graph = effect(CLSID_D2D1Border, "Grain", vec![Prop::U32(WRAP), Prop::U32(WRAP)], vec![param(NOISE)?], Vec::new());
                    let f = compile(c, &graph, &[])?;
                    let tile = c.CreateSurfaceBrushWithSurface(&tile_surface()?)?;
                    tile.SetStretch(CompositionStretch::None)?;
                    Ok((f, tile))
                })();
                k.grain = Some(Grain { generation: now, made: made.map_err(|e| crate::log!("glass: no grain ({e})")).ok() });
            }
            GrainStep::Refill => {
                if let Some(Grain { generation, made: Some((_, tile)) }) = k.grain.as_mut() {
                    match tile_surface().and_then(|s| tile.SetSurface(&s)) {
                        Ok(()) => *generation = now,
                        // Kept as it was: tried again next time.
                        Err(e) => crate::log!("glass: grain not filled again ({e})"),
                    }
                }
            }
        }
        k.grain.as_ref().and_then(|g| g.made.clone())
    })?;
    let (f, tile) = made;
    let b = f.CreateBrush().ok()?;
    b.SetSourceParameter(&HSTRING::from(NOISE), &tile).ok()?;
    b.cast().ok()
}

/// Pure: what top_light lays over material `m`'s tint: (the top light,
/// the grain). A glass's own; Flat v2's grain alone (no top light: an
/// opaque plate is lit by its gradient and its rim); else none.
pub fn light_of(theme: &Theme, m: Material) -> (f32, f32) {
    match theme.glass_fx(m) {
        Some(fx) => (fx.spec, fx.noise),
        None => (0.0, theme.flat_grain(m)),
    }
}

/// The top light and grain over material `m`'s tint (light_of): with a
/// glass in force the top light (white at the glass's `spec` at the top,
/// gone at the bottom: SPEC_LINE) and the grain
/// (at its `noise`); on Flat v2 its grain. `size`: the plate's (None: its
/// parent's, relatively). None when there is neither (high contrast, name
/// labels, a Flat the PC forces).
pub fn top_light(gfx: &Gfx, theme: &Theme, m: Material, size: Option<Vector2>) -> Result<Option<ContainerVisual>> {
    let (spec, noise) = light_of(theme, m);
    let fx = GlassFx { sat: 1.0, lum: Rgba { r: 0, g: 0, b: 0, a: 0 }, spec, noise };
    if fx.spec <= 0.0 && fx.noise <= 0.0 {
        return Ok(None);
    }
    let c = &gfx.compositor;
    let root = c.CreateContainerVisual()?;
    match size {
        Some(s) => root.SetSize(s)?,
        None => root.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?,
    }
    let white = |a: f32| Color { A: (a.clamp(0.0, 1.0) * 255.0).round() as u8, R: 255, G: 255, B: 255 };
    let layer = |brush: &CompositionBrush, opacity: f32| -> Result<()> {
        let v = c.CreateSpriteVisual()?;
        v.SetBrush(brush)?;
        v.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;
        v.SetOpacity(opacity)?;
        root.Children()?.InsertAtTop(&v)?;
        Ok(())
    };
    if fx.spec > 0.0 {
        let lin = c.CreateLinearGradientBrush()?;
        lin.SetStartPoint(Vector2 { X: 0.0, Y: 0.0 })?;
        lin.SetEndPoint(Vector2 { X: 0.0, Y: 1.0 })?;
        for (at, a) in SPEC_LINE {
            lin.ColorStops()?.Append(&c.CreateColorGradientStopWithOffsetAndColor(at, white(fx.spec * a))?)?;
        }
        layer(&lin.cast()?, 1.0)?;
    }
    if fx.spec > 0.0 && SPEC_GLOW.2 > 0.0 {
        let glow = c.CreateRadialGradientBrush()?;
        glow.SetEllipseCenter(Vector2 { X: 0.5, Y: 0.0 })?;
        glow.SetEllipseRadius(Vector2 { X: SPEC_GLOW.0, Y: SPEC_GLOW.1 })?;
        glow.ColorStops()?.Append(&c.CreateColorGradientStopWithOffsetAndColor(0.0, white(fx.spec * SPEC_GLOW.2))?)?;
        glow.ColorStops()?.Append(&c.CreateColorGradientStopWithOffsetAndColor(1.0, white(0.0))?)?;
        layer(&glow.cast()?, 1.0)?;
    }
    if fx.noise > 0.0 {
        if let Some(b) = grain(gfx) {
            layer(&b, fx.noise)?;
        }
    }
    Ok(Some(root))
}

/// The top light's fall: (offset down the plate, share of `spec`). Glass
/// A (2026-11-12): over the whole plate, gone at its bottom.
pub const SPEC_LINE: [(f32, f32); 2] = [(0.0, 1.0), (1.0, 0.0)];
/// The top light's glow at the top's middle: its radii (share of the
/// plate's width and height) and its share of `spec`. Glass A has none
/// (0: not drawn).
pub const SPEC_GLOW: (f32, f32, f32) = (0.72, 0.36, 0.0);

// ---- the graph, on the CPU (Theme::plate_seen: text contrast; render tests) ----

/// Pure: `c` (sRGB, 0..1) through D2D's Saturation effect at `s` (its
/// matrix; 1: as it is). Not clamped: the compositor carries the values
/// past 0 and 1 into the next effect (read back: the Luminosity layer's
/// ClipColor brings them in, model() clamps at the end).
pub fn saturate(c: [f32; 3], s: f32) -> [f32; 3] {
    let [r, g, b] = c;
    let (wr, wg, wb) = (0.213, 0.715, 0.072);
    let ch = |own: f32, k: f32| own * (k + (1.0 - k) * s);
    let mix = |w: f32, v: f32| w * (1.0 - s) * v;
    [ch(r, wr) + mix(wg, g) + mix(wb, b), mix(wr, r) + ch(g, wg) + mix(wb, b), mix(wr, r) + mix(wg, g) + ch(b, wb)]
}

/// The blend modes' lightness (W3C compositing, as D2D's Blend).
fn lum_of(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

/// Pure: `c` given lightness `l`, its hue and saturation kept (W3C
/// SetLum with ClipColor).
pub fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum_of(c);
    let c = c.map(|v| v + d);
    let l = lum_of(c);
    let (n, x) = (c[0].min(c[1]).min(c[2]), c[0].max(c[1]).max(c[2]));
    c.map(|v| {
        let mut v = v;
        if n < 0.0 {
            v = l + (v - l) * l / (l - n).max(1e-6);
        }
        if x > 1.0 {
            v = l + (v - l) * (1.0 - l) / (x - l).max(1e-6);
        }
        v.clamp(0.0, 1.0)
    })
}

/// Pure: one pixel of a plate: backdrop `bg` (sRGB 0..1, already blurred)
/// through the glass `fx` (saturation, the Luminosity layer) and `tint`
/// alpha-blended over it: the graph's formulas (what text contrast is
/// measured against, Theme::plate_seen, and what the render tests draw
/// instead of DWM's backdrop, which no off-screen picture has).
pub fn model(bg: [f32; 3], fx: &GlassFx, tint: Rgba) -> [f32; 3] {
    let s = saturate(bg, fx.sat);
    let (g, la) = (fx.lum.r as f32 / 255.0, fx.lum.a as f32 / 255.0);
    let lit = set_lum(s, g);
    let l: [f32; 3] = std::array::from_fn(|i| s[i] * (1.0 - la) + lit[i] * la);
    let (t, ta) = (tint.r as f32 / 255.0, tint.a as f32 / 255.0);
    l.map(|v| (v.clamp(0.0, 1.0) * (1.0 - ta) + t * ta).clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syslook::SysLook;
    use crate::theme::Finish;

    fn glass(finish: Finish, light: bool) -> Theme {
        let mut t = Theme::graphite();
        t.finish = finish;
        t.resolve_fx(&SysLook { light, ..Default::default() }, true)
    }

    #[test]
    fn saturation_is_d2ds_matrix() {
        let c = [0.8, 0.4, 0.2];
        // 1: as it is; 0: its luma grey (D2D's weights).
        assert!(saturate(c, 1.0).iter().zip(c).all(|(a, b)| (a - b).abs() < 1e-6));
        let g = 0.213 * 0.8 + 0.715 * 0.4 + 0.072 * 0.2;
        assert!(saturate(c, 0.0).iter().all(|v| (v - g).abs() < 1e-6));
        // Above 1 the channels part further; a grey stays grey.
        let s = saturate(c, 1.5);
        assert!(s[0] - s[2] > c[0] - c[2]);
        assert_eq!(saturate([0.5; 3], 2.0).map(|v| (v * 1000.0).round()), [500.0; 3]);
    }

    #[test]
    fn the_luminosity_layer_sets_lightness_and_keeps_color() {
        let warm = [110.0 / 255.0, 90.0 / 255.0, 70.0 / 255.0];
        let out = set_lum(warm, 0.15);
        assert!((lum_of(out) - 0.15).abs() < 0.01, "{out:?}");
        // The hue's order stays, the channels still apart.
        assert!(out[0] > out[1] && out[1] > out[2]);
        // A grey takes the grey's lightness exactly.
        assert_eq!(set_lum([0.5; 3], 0.2).map(|v| (v * 100.0).round()), [20.0; 3]);
        // So over a grey backdrop the layer is an alpha blend of the grey:
        // tint() may fold it into the tint.
        let fx = GlassFx { sat: 2.0, lum: Rgba { r: 0x26, g: 0x26, b: 0x26, a: 89 }, spec: 0.0, noise: 0.0 };
        let tint = Rgba { r: 0x26, g: 0x26, b: 0x26, a: 148 };
        let folded = fold(tint, fx.lum);
        for v in [0.0f32, 0.5, 1.0] {
            let a = model([v; 3], &fx, tint)[0];
            let b = model([v; 3], &GlassFx { lum: Rgba { a: 0, ..fx.lum }, ..fx }, folded)[0];
            assert!((a - b).abs() < 0.01, "{v}: {a} {b}");
        }
    }

    /// The point of glass v3: over a colored backdrop far more of the
    /// color is left than the old alpha plate left (the research's .12 to
    /// .26), at about the same lightness.
    #[test]
    fn the_glass_keeps_the_wallpapers_color() {
        let warm = [110.0 / 255.0, 90.0 / 255.0, 70.0 / 255.0];
        let spread = |c: [f32; 3]| c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2]);
        // The old dark panel: #262626 at .84, alpha alone.
        let old = model(warm, &GlassFx { sat: 1.0, lum: Rgba { r: 0x26, g: 0x26, b: 0x26, a: 0 }, spec: 0.0, noise: 0.0 }, Rgba { r: 0x26, g: 0x26, b: 0x26, a: 214 });
        for f in [Finish::Glass] {
            let t = glass(f, false);
            let fx = t.glass_fx(Material::Panel).unwrap();
            let new = model(warm, &fx, t.material(Material::Panel));
            assert!(spread(new) > spread(old) * 2.5, "{f:?}: {new:?} vs {old:?}");
            // Achromatic paint: the tint and the layer are greys.
            let (lum, tint) = (fx.lum, t.material(Material::Panel));
            assert!(lum.r == lum.g && lum.g == lum.b && tint.r == tint.g && tint.g == tint.b);
        }
    }

    #[test]
    fn the_grain_tile_is_grey_and_even() {
        let px = noise_tile();
        assert_eq!(px.len(), (TILE * TILE * 4) as usize);
        let mut sum = 0u64;
        for p in px.chunks(4) {
            // Premultiplied grey at half lightness.
            assert!(p[0] == p[1] && p[1] == p[2] && p[0] as u32 <= p[3] as u32);
            assert!((p[0] as i32 - p[3] as i32 / 2).abs() <= 1);
            sum += p[3] as u64;
        }
        let mean = sum as f32 / (TILE * TILE) as f32;
        assert!((110.0..145.0).contains(&mean), "{mean}");
        assert_eq!(px, noise_tile(), "the same every time");
    }

    #[test]
    fn only_a_glass_has_fx() {
        for light in [false, true] {
            assert!(Material::ALL.iter().all(|&m| glass(Finish::Flat, light).glass_fx(m).is_none()));
            for f in [Finish::Glass] {
                let t = glass(f, light);
                assert!(t.glass_fx(Material::Tip).is_none(), "labels have no backdrop");
                for m in [Material::Bar, Material::Dock, Material::Panel, Material::Menu] {
                    let fx = t.glass_fx(m).unwrap();
                    assert!((1.0..=2.0).contains(&fx.sat) && fx.lum.a > 0, "{f:?} {m:?}");
                    // Its grey is the material's own.
                    assert_eq!(fx.lum.r, t.material(m).r);
                }
            }
        }
        // High contrast: none.
        let mut raw = Theme::graphite();
        raw.finish = Finish::Glass;
        let hc = crate::syslook::HighContrast { window: Rgba { r: 0, g: 0, b: 0, a: 255 }, text: Rgba { r: 255, g: 255, b: 255, a: 255 }, highlight: Rgba { r: 255, g: 255, b: 0, a: 255 }, highlight_text: Rgba { r: 0, g: 0, b: 0, a: 255 }, gray_text: crate::syslook::GRAY_TEXT };
        let t = raw.resolve_fx(&SysLook { high_contrast: Some(hc), ..Default::default() }, true);
        assert!(Material::ALL.iter().all(|&m| t.glass_fx(m).is_none()));
    }

    // ---- review 1: the fallback is decided by the brush made ------------------

    /// What ladder() makes when `fails` fail and `missing` cannot be had.
    fn climb(fails: &[Level], missing: &[Level]) -> Option<Level> {
        ladder(|level| {
            if missing.contains(&level) {
                return None;
            }
            Some(if fails.contains(&level) { Err(Error::from(E_UNEXPECTED)) } else { Ok(level) })
        })
        .map(|(l, made)| {
            assert_eq!(l, made, "the level it says is the one it made");
            l
        })
    }

    #[test]
    fn a_failed_brush_falls_one_level_at_a_time() {
        // Nothing fails: the full graph.
        assert_eq!(climb(&[], &[]), Some(Level::Full));
        // A brush of the full graph fails (made, its source, its
        // properties: any step): saturation alone.
        assert_eq!(climb(&[Level::Full], &[]), Some(Level::Sat));
        // Its factory could not be had: the same, without a log line.
        assert_eq!(climb(&[], &[Level::Full]), Some(Level::Sat));
        // Both fail, or neither can be had: the plain backdrop.
        assert_eq!(climb(&[Level::Full, Level::Sat], &[]), None);
        assert_eq!(climb(&[Level::Sat], &[Level::Full]), None);
        assert_eq!(climb(&[], &[Level::Full, Level::Sat]), None);
        // Saturation alone failing does not cost the full graph.
        assert_eq!(climb(&[Level::Sat], &[]), Some(Level::Full));
    }

    #[test]
    fn the_brush_carries_its_level() {
        for l in [Level::Full, Level::Sat] {
            assert_eq!(Level::of_tag(l.tag()), l);
        }
        // Any other brush (the host backdrop, a color): plain.
        for other in ["", "glass-top-light", "Glass-Full", "something"] {
            assert_eq!(Level::of_tag(other), Level::Plain, "{other}");
        }
        assert!(usable(CompositionEffectFactoryLoadStatus::Success) && usable(CompositionEffectFactoryLoadStatus::Pending));
        assert!(!usable(CompositionEffectFactoryLoadStatus::EffectTooComplex) && !usable(CompositionEffectFactoryLoadStatus::Other));
    }

    /// Review 1's point: the tint and the effect always agree. At every
    /// level a brush can end up at, the tint chosen for it gives the plate
    /// the full graph's lightness over a grey backdrop (so text keeps its
    /// contrast), and only the full graph leaves the tint as it is.
    #[test]
    fn the_tint_follows_the_level_the_brush_draws() {
        for light in [false, true] {
            for f in [Finish::Glass] {
                let t = glass(f, light);
                for m in [Material::Bar, Material::Dock, Material::Panel, Material::Menu] {
                    let fx = t.glass_fx(m).unwrap();
                    assert_eq!(tint_at(&t, m, Level::Full), t.material(m));
                    for l in [Level::Sat, Level::Plain] {
                        assert_eq!(tint_at(&t, m, l), fold(t.material(m), fx.lum), "{f:?} {m:?} {l:?}");
                    }
                    for v in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
                        let full = model([v; 3], &fx, tint_at(&t, m, Level::Full));
                        for l in [Level::Sat, Level::Plain] {
                            let got = model([v; 3], &l.fx(fx), tint_at(&t, m, l));
                            assert!((full[0] - got[0]).abs() < 0.01, "{f:?} light {light} {m:?} {l:?} over {v}: {full:?} vs {got:?}");
                        }
                    }
                }
                // Labels and Flat: no glass, nothing folded at any level.
                for l in [Level::Full, Level::Sat, Level::Plain] {
                    assert_eq!(tint_at(&t, Material::Tip, l), t.material(Material::Tip));
                    let flat = glass(Finish::Flat, light);
                    assert_eq!(tint_at(&flat, Material::Panel, l), flat.material(Material::Panel));
                }
            }
        }
        // A level draws no more than it is: saturation only from Sat up,
        // the Luminosity layer only on Full.
        let fx = GlassFx { sat: 2.0, lum: Rgba { r: 0x26, g: 0x26, b: 0x26, a: 200 }, spec: 0.05, noise: 0.02 };
        assert_eq!(Level::Full.fx(fx), fx);
        assert_eq!((Level::Sat.fx(fx).sat, Level::Sat.fx(fx).lum.a), (2.0, 0));
        assert_eq!((Level::Plain.fx(fx).sat, Level::Plain.fx(fx).lum.a), (1.0, 0));
    }

    // ---- review 2: the effect objects' COM side --------------------------------

    /// `u`'s reference count now (AddRef then Release: what Release says).
    fn refs(u: &windows::core::IUnknown) -> u32 {
        unsafe {
            (Interface::vtable(u).AddRef)(Interface::as_raw(u));
            (Interface::vtable(u).Release)(Interface::as_raw(u))
        }
    }

    fn sample() -> IGraphicsEffect {
        let leaf = lum_flood();
        sat_effect(leaf.cast().unwrap())
    }

    #[test]
    fn an_effect_answers_its_interfaces_as_one_object() {
        let e = sample();
        let unknown: windows::core::IUnknown = e.cast().unwrap();
        let source: IGraphicsEffectSource = e.cast().unwrap();
        let interop: IGraphicsEffectD2D1Interop = e.cast().unwrap();
        // One identity whichever interface it is asked through.
        assert_eq!(source.cast::<windows::core::IUnknown>().unwrap(), unknown);
        assert_eq!(interop.cast::<windows::core::IUnknown>().unwrap(), unknown);
        assert_eq!(interop.cast::<IGraphicsEffect>().unwrap(), e);
        // Agile (what makes the name's lock needed): any thread may call it.
        assert!(e.cast::<windows::Win32::System::Com::IAgileObject>().is_ok());
        // Not something it is not.
        assert!(e.cast::<windows::Foundation::IStringable>().is_err());
    }

    #[test]
    fn an_effect_counts_its_references_and_lets_its_sources_go() {
        let leaf = lum_flood();
        let leaf_unknown: windows::core::IUnknown = leaf.cast().unwrap();
        // leaf + leaf_unknown.
        assert_eq!(refs(&leaf_unknown), 2);
        let top = sat_effect(leaf.cast().unwrap());
        // The effect over it keeps one.
        assert_eq!(refs(&leaf_unknown), 3);
        let top_unknown: windows::core::IUnknown = top.cast().unwrap();
        assert_eq!(refs(&top_unknown), 2);
        {
            // A source handed out is a reference of the caller's own.
            let interop: IGraphicsEffectD2D1Interop = top.cast().unwrap();
            let s = unsafe { interop.GetSource(0) }.unwrap();
            assert_eq!(s.cast::<windows::core::IUnknown>().unwrap(), leaf_unknown);
            assert_eq!(refs(&leaf_unknown), 4);
            assert_eq!(refs(&top_unknown), 3);
        }
        assert_eq!(refs(&leaf_unknown), 3);
        assert_eq!(refs(&top_unknown), 2);
        // The effect gone, its source is let go.
        drop(top);
        drop(top_unknown);
        assert_eq!(refs(&leaf_unknown), 2);
    }

    #[test]
    fn an_effects_properties_map_as_d2d_wants_them() {
        use windows::Foundation::PropertyType;
        let blend: IGraphicsEffectD2D1Interop = plate_graph_of(sample()).cast().unwrap();
        let sat: IGraphicsEffectD2D1Interop = sat_effect(lum_flood().cast().unwrap()).cast().unwrap();
        let flood: IGraphicsEffectD2D1Interop = lum_flood().cast().unwrap();
        unsafe {
            assert_eq!(sat.GetEffectId().unwrap(), CLSID_D2D1Saturation);
            assert_eq!(flood.GetEffectId().unwrap(), CLSID_D2D1Flood);
            assert_eq!(blend.GetEffectId().unwrap(), CLSID_D2D1Blend);
            // The names a brush sets, any case: index and mapping.
            let (mut index, mut mapping) = (99u32, GRAPHICS_EFFECT_PROPERTY_MAPPING(-1));
            sat.GetNamedPropertyMapping(windows::core::w!("saturation"), &mut index, &mut mapping).unwrap();
            assert_eq!((index, mapping), (0, GRAPHICS_EFFECT_PROPERTY_MAPPING_DIRECT));
            flood.GetNamedPropertyMapping(windows::core::w!("Color"), &mut index, &mut mapping).unwrap();
            assert_eq!((index, mapping), (0, GRAPHICS_EFFECT_PROPERTY_MAPPING_COLOR_TO_VECTOR4));
            // Null out-pointers are skipped, not written through.
            sat.GetNamedPropertyMapping(windows::core::w!("Saturation"), std::ptr::null_mut(), std::ptr::null_mut()).unwrap();
            // An unknown name, a null name: E_INVALIDARG; the blend has none.
            assert_eq!(sat.GetNamedPropertyMapping(windows::core::w!("Hue"), &mut index, &mut mapping).unwrap_err().code(), E_INVALIDARG);
            assert_eq!(sat.GetNamedPropertyMapping(PCWSTR::null(), &mut index, &mut mapping).unwrap_err().code(), E_INVALIDARG);
            assert_eq!(blend.GetNamedPropertyMapping(windows::core::w!("Mode"), &mut index, &mut mapping).unwrap_err().code(), E_INVALIDARG);
            // The properties by index, of D2D's types.
            assert_eq!(sat.GetPropertyCount().unwrap(), 1);
            let p = sat.GetProperty(0).unwrap();
            assert_eq!((p.Type().unwrap(), p.GetSingle().unwrap()), (PropertyType::Single, 1.0));
            let p = blend.GetProperty(0).unwrap();
            assert_eq!((p.Type().unwrap(), p.GetUInt32().unwrap()), (PropertyType::UInt32, LUM_MODE));
            let p = flood.GetProperty(0).unwrap();
            assert_eq!(p.Type().unwrap(), PropertyType::SingleArray);
            let mut v = windows::core::Array::<f32>::new();
            p.GetSingleArray(&mut v).unwrap();
            assert_eq!(&v[..], &[0.0; 4]);
            assert_eq!(sat.GetProperty(1).unwrap_err().code(), E_BOUNDS);
            // The inputs, in order (the blend: background, then the layer).
            assert_eq!((blend.GetSourceCount().unwrap(), sat.GetSourceCount().unwrap(), flood.GetSourceCount().unwrap()), (2, 1, 0));
            assert_eq!(blend.GetSource(1).unwrap().cast::<IGraphicsEffect>().unwrap().Name().unwrap(), "LumColor");
            assert_eq!(flood.GetSource(0).unwrap_err().code(), E_BOUNDS);
        }
    }

    /// The Blend of plate_graph() over `sat` (no CompositionEffectSource-
    /// Parameter: the test needs no compositor).
    fn plate_graph_of(sat: IGraphicsEffect) -> IGraphicsEffect {
        effect(CLSID_D2D1Blend, "Lum", vec![Prop::U32(LUM_MODE)], vec![sat.cast().unwrap(), lum_flood().cast().unwrap()], Vec::new())
    }

    #[test]
    fn an_effects_name_is_safe_from_any_thread() {
        // The object is agile: threads may read and set its name at once.
        struct Shared(IGraphicsEffect);
        unsafe impl Send for Shared {}
        unsafe impl Sync for Shared {}
        let e = std::sync::Arc::new(Shared(sample()));
        let names = ["Sat", "a", "a longer name", "z"];
        let threads: Vec<_> = (0..4)
            .map(|i| {
                let e = e.clone();
                std::thread::spawn(move || {
                    for k in 0..2000 {
                        if (k + i) % 3 == 0 {
                            e.0.SetName(&HSTRING::from(names[(k + i) % names.len()])).unwrap();
                        } else {
                            let n = e.0.Name().unwrap().to_string();
                            assert!(names.contains(&n.as_str()), "{n:?}");
                        }
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().expect("no thread failed");
        }
        e.0.SetName(&HSTRING::from("Sat")).unwrap();
        assert_eq!(e.0.Name().unwrap(), "Sat");
    }

    // ---- review 4: the grain after a device recovery ---------------------------

    #[test]
    fn the_grain_is_filled_again_on_a_new_device() {
        // Nothing yet: made.
        assert_eq!(grain_step(None, 0), GrainStep::Make);
        // Made on this device: used as it is.
        assert_eq!(grain_step(Some((3, true)), 3), GrainStep::Use);
        // The device was swapped (its pixels are gone): filled again.
        assert_eq!(grain_step(Some((3, true)), 4), GrainStep::Refill);
        // Could not be made on this device: not tried again on it, but on
        // the next one (a loss while making it is not for good).
        assert_eq!(grain_step(Some((3, false)), 3), GrainStep::Use);
        assert_eq!(grain_step(Some((3, false)), 4), GrainStep::Make);
        // Generations wrap.
        assert_eq!(grain_step(Some((u32::MAX, true)), 0), GrainStep::Refill);
    }
}

#[cfg(test)]
#[path = "glass_harmony.rs"]
mod glass_harmony;
