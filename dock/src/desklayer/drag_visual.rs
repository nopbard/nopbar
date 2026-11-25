//! Retained visuals for a widget drag. All animated geometry is in screen pixels.
use super::{backdrop, card, edit::Preview, edit_view, widget::Size};
use crate::{
    gfx::Gfx,
    monitors::Monitor,
    motion,
    theme::{Rgba, Theme},
};
use std::{
    cell::RefCell,
    collections::VecDeque,
    rc::Rc,
    sync::{Arc, Weak},
};
use windows::{
    core::{Interface, Result, HSTRING},
    Foundation::TimeSpan,
    Win32::Foundation::{HWND, RECT},
    UI::{Color, Composition::*},
};
use windows_numerics::{Matrix3x2, Vector2, Vector3};

const LIFT_SLACK: motion::Spring = motion::Spring {
    mass: 1.,
    stiffness: 2304.,
    damping: 96.,
};
pub(super) const LIFT: motion::Spring = motion::Spring {
    mass: 1.,
    stiffness: 1600.,
    damping: 54.,
};
pub(super) const LIFT_MS: i64 = 180;
pub(super) const DROP_MS: i64 = 300;
const TARGET_MS: i64 = 220;
const LIFT_SLACK_MS: i64 = 150;
pub(super) const DOT_MS: i64 = 120;
pub(super) const LIFT_SCALE: f32 = 1.04;
const SHADOW_PAD: f32 = 64.;

fn xy(x: f32, y: f32) -> Vector2 {
    Vector2 { X: x, Y: y }
}
fn xyz(x: f32, y: f32, z: f32) -> Vector3 {
    Vector3 { X: x, Y: y, Z: z }
}
fn pos(r: RECT) -> Vector3 {
    xyz(r.left as f32, r.top as f32, 0.)
}
fn dims(r: RECT) -> Vector2 {
    xy((r.right - r.left) as f32, (r.bottom - r.top) as f32)
}
fn centre(r: RECT) -> Vector3 {
    let (p, d) = (pos(r), dims(r));
    xyz(p.X + d.X / 2., p.Y + d.Y / 2., 0.)
}
fn hand_position(r: RECT, offset: (f32, f32)) -> Vector3 {
    pos(r) + xyz(offset.0, offset.1, 0.)
}
fn color(c: Rgba) -> Color {
    Color {
        R: c.r,
        G: c.g,
        B: c.b,
        A: c.a,
    }
}

fn bind(
    c: &Compositor,
    obj: &impl Interface,
    name: &str,
    expr: &str,
    props: &CompositionPropertySet,
) -> Result<()> {
    let a = c.CreateExpressionAnimationWithExpression(&HSTRING::from(expr))?;
    a.SetReferenceParameter(&HSTRING::from("p"), props)?;
    obj.cast::<CompositionObject>()?
        .StartAnimation(&HSTRING::from(name), &a)
}

fn bind_glass(
    c: &Compositor,
    brush: &CompositionSurfaceBrush,
    props: &CompositionPropertySet,
    half: Vector2,
    wallpaper: &WallpaperSurface,
) -> Result<()> {
    // Undo the card's motion in its brush coordinates so the wallpaper
    // stays fixed in screen space while the card is scaled or gliding.
    let grow = LIFT_SCALE - 1.;
    let scale = |axis| format!("(p.baseScale.{axis}*(1+p.lift*{grow:.4}))");
    let (sx, sy) = (scale("X"), scale("Y"));
    bind(
        c,
        brush,
        "Offset",
        &format!(
            "Vector2(({}-p.hand.X-p.slack.X-{:.2}*p.baseScale.X)/{sx}+{:.2}, ({}-p.hand.Y-p.slack.Y-{:.2}*p.baseScale.Y)/{sy}+{:.2})",
            wallpaper.rect.left, half.X, half.X, wallpaper.rect.top, half.Y, half.Y
        ),
        props,
    )?;
    bind(
        c,
        brush,
        "Scale",
        &format!(
            "Vector2({:.6}/{sx}, {:.6}/{sy})",
            wallpaper.scale.X, wallpaper.scale.Y
        ),
        props,
    )
}

pub(super) struct CardVisual {
    pub root: ContainerVisual,
    glass: Option<(CompositionSurfaceBrush, Rc<WallpaperSurface>)>,
}

impl Drop for CardVisual {
    fn drop(&mut self) {
        if let Some((brush, _)) = &self.glass {
            let _ = brush.StopAnimation(&HSTRING::from("Offset"));
            let _ = brush.StopAnimation(&HSTRING::from("Scale"));
            let _ = brush.Close();
        }
        close_children(&self.root);
        let _ = self.root.Close();
    }
}

fn close_children(root: &ContainerVisual) {
    if let Ok(children) = root.Children() {
        let detached: Vec<_> = (&children).into_iter().collect();
        let _ = children.RemoveAll();
        for visual in detached {
            let _ = visual.Close();
        }
    }
}

struct WallpaperSurface {
    surface: CompositionDrawingSurface,
    rect: RECT,
    scale: Vector2,
}

impl Drop for WallpaperSurface {
    fn drop(&mut self) {
        let _ = self.surface.Close();
    }
}

struct CacheEntry {
    compositor: Compositor,
    picture: Weak<backdrop::Grey>,
    tint: Rgba,
    image: Rc<WallpaperSurface>,
}

thread_local! {
    static GLASS_CACHE: RefCell<VecDeque<CacheEntry>> = const { RefCell::new(VecDeque::new()) };
}

fn glass_surface(
    gfx: &Gfx,
    grey: &Arc<backdrop::Grey>,
    tint: Rgba,
) -> Result<Rc<WallpaperSurface>> {
    const CACHE_LIMIT: usize = 8;
    let picture = Arc::downgrade(grey);
    GLASS_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache
            .retain(|entry| entry.picture.strong_count() > 0 && entry.compositor == gfx.compositor);
        if let Some(i) = cache.iter().position(|entry| {
            entry.image.rect == grey.mon && entry.picture.ptr_eq(&picture) && entry.tint == tint
        }) {
            let entry = cache.remove(i).unwrap();
            let image = entry.image.clone();
            cache.push_back(entry);
            return Ok(image);
        }
        // Upload the already blurred mica at source resolution.
        let coarse = &grey.coarse;
        let mut pixels = coarse.pixels.clone();
        for px in pixels.as_chunks_mut::<4>().0 {
            px[3] = 128;
        }
        card::tinted(&mut pixels, tint);
        let surface = gfx.surface_from_bgra(coarse.width, coarse.height, &pixels)?;
        let d = dims(grey.mon);
        let image = Rc::new(WallpaperSurface {
            surface,
            rect: grey.mon,
            scale: xy(d.X / coarse.width as f32, d.Y / coarse.height as f32),
        });
        if cache.len() == CACHE_LIMIT {
            cache.pop_front();
        }
        cache.push_back(CacheEntry {
            compositor: gfx.compositor.clone(),
            picture,
            tint,
            image: image.clone(),
        });
        Ok(image)
    })
}

fn surface(c: &Compositor, s: &CompositionDrawingSurface, size: Vector2) -> Result<SpriteVisual> {
    let v = c.CreateSpriteVisual()?;
    v.SetSize(size)?;
    v.SetBrush(&c.CreateSurfaceBrushWithSurface(s)?)?;
    Ok(v)
}

fn rounded(
    c: &Compositor,
    v: &SpriteVisual,
    size: Vector2,
    radius: f32,
) -> Result<CompositionRoundedRectangleGeometry> {
    let geometry = c.CreateRoundedRectangleGeometry()?;
    geometry.SetSize(size)?;
    geometry.SetCornerRadius(xy(radius, radius))?;
    v.SetClip(&c.CreateGeometricClipWithGeometry(&geometry)?)?;
    Ok(geometry)
}

fn vector_spring(
    c: &Compositor,
    p: &CompositionPropertySet,
    name: &str,
    to: Vector3,
    spring: motion::Spring,
    ms: i64,
) -> Result<()> {
    let a = keyed_vector(c, spring, ms)?;
    retarget(p, name, to, &a)
}

fn keyed_vector(
    c: &Compositor,
    spring: motion::Spring,
    ms: i64,
) -> Result<Vector3KeyFrameAnimation> {
    let a = c.CreateVector3KeyFrameAnimation()?;
    let linear = c.CreateLinearEasingFunction()?;
    a.SetDuration(TimeSpan {
        Duration: crate::effects::ms(ms) * 10_000,
    })?;
    for (t, portion) in spring
        .keys(ms, motion::SAMPLES)
        .into_iter()
        .take(motion::SAMPLES)
    {
        let key = format!("this.StartingValue + (destination - this.StartingValue) * {portion:.6}");
        a.InsertExpressionKeyFrameWithEasingFunction(t, &HSTRING::from(key), &linear)?;
    }
    a.InsertExpressionKeyFrameWithEasingFunction(1., &HSTRING::from("destination"), &linear)?;
    Ok(a)
}

fn retarget(
    p: &CompositionPropertySet,
    name: &str,
    to: Vector3,
    a: &Vector3KeyFrameAnimation,
) -> Result<()> {
    let name = HSTRING::from(name);
    if !motion::full() {
        p.StopAnimation(&name)?;
        return p.InsertVector3(&name, to);
    }
    a.SetVector3Parameter(&HSTRING::from("destination"), to)?;
    p.StartAnimation(&name, a)
}

pub(super) const STILL_LANDING: motion::Spring = motion::Spring {
    mass: 1.,
    stiffness: 2304.,
    damping: 96.,
};

struct LandingKey {
    progress: f32,
    position: Vector3,
    start_weight: f32,
}

fn landing_keys(
    to: Vector3,
    velocity: (f32, f32),
    scale: f32,
    duration_ms: i64,
) -> impl Iterator<Item = LandingKey> {
    let speed = velocity.0.hypot(velocity.1) / scale;
    let ratio = 1. - 0.12 * ((speed - 100.) / 900.).clamp(0., 1.);
    let spring = motion::Spring { damping: STILL_LANDING.damping * ratio, ..STILL_LANDING };
    let capped = 1600. / speed.max(1600.);
    // Map physical velocity to nominal time so the setting that changes
    // animation duration does not introduce a jump in initial speed.
    let speed_scale = capped * duration_ms as f32 / DROP_MS as f32;
    let velocity = (velocity.0 * speed_scale, velocity.1 * speed_scale);
    let intervals = ((duration_ms + 4) / 5).max(24);
    (0..=intervals).map(move |i| {
        let progress = i as f32 / intervals as f32;
        if i == intervals {
            return LandingKey { progress, position: to, start_weight: 0. };
        }
        let t = progress * DROP_MS as f32 / 1000.;
        LandingKey {
            progress,
            position: xyz(spring.at(0., velocity.0, to.X, t).0, spring.at(0., velocity.1, to.Y, t).0, 0.),
            start_weight: spring.at(1., 0., 0., t).0,
        }
    })
}

fn animate_slack(
    c: &Compositor,
    props: &CompositionPropertySet,
    to: Vector3,
    velocity: (f32, f32),
    scale: f32,
) -> Result<()> {
    let slack = HSTRING::from("slack");
    if !motion::full() {
        props.StopAnimation(&slack)?;
        return props.InsertVector3(&slack, to);
    }
    let duration = crate::effects::ms(DROP_MS);
    let animation = c.CreateVector3KeyFrameAnimation()?;
    let linear = c.CreateLinearEasingFunction()?;
    animation.SetDuration(TimeSpan { Duration: duration * 10_000 })?;
    for key in landing_keys(to, velocity, scale, duration) {
        if key.progress == 1. {
            animation.InsertKeyFrameWithEasingFunction(1., to, &linear)?;
        } else {
            // The lift's slack may still be animating. The solution is
            // linear in initial position, so use the compositor's value
            // at the handoff for that term.
            let value = format!("Vector3({:.7}, {:.7}, 0) + this.StartingValue * {:.8}",
                key.position.X, key.position.Y, key.start_weight);
            animation.InsertExpressionKeyFrameWithEasingFunction(key.progress, &HSTRING::from(value), &linear)?;
        }
    }
    props.StartAnimation(&slack, &animation)
}

fn lift(c: &Compositor, p: &CompositionPropertySet, up: bool) -> Result<()> {
    if !motion::full() {
        return Ok(());
    }
    let (spring, ms, end) = if up {
        (LIFT, LIFT_MS, 1.)
    } else {
        (STILL_LANDING, DROP_MS, 0.)
    };
    let a = c.CreateScalarKeyFrameAnimation()?;
    let linear = c.CreateLinearEasingFunction()?;
    a.SetDuration(TimeSpan {
        Duration: crate::effects::ms(ms) * 10_000,
    })?;
    for (t, portion) in spring
        .keys(ms, motion::SAMPLES)
        .into_iter()
        .take(motion::SAMPLES)
    {
        let key = format!("this.StartingValue + ({end:.1} - this.StartingValue) * {portion:.6}");
        a.InsertExpressionKeyFrameWithEasingFunction(t, &HSTRING::from(key), &linear)?;
    }
    a.InsertKeyFrame(1., end)?;
    p.StartAnimation(&HSTRING::from("lift"), &a)
}

pub(super) struct Face {
    bitmap: CompositionDrawingSurface,
    rest_shadow: CompositionDrawingSurface,
    drag_shadow: CompositionDrawingSurface,
    props: CompositionPropertySet,
    size: Vector2,
    dpi: f32,
    light: bool,
    glass: Option<crate::theme::AdaptiveGlass>,
    live: bool,
}

impl Drop for Face {
    fn drop(&mut self) {
        for surface in [&self.bitmap, &self.rest_shadow, &self.drag_shadow] {
            let _ = surface.Close();
        }
    }
}

impl Face {
    pub fn new(
        gfx: &Gfx,
        theme: &Theme,
        preview: &Preview,
        size: Size,
        at: RECT,
        dpi: f32,
        under: Option<&backdrop::Under>,
    ) -> Result<Self> {
        let d = dims(at);
        let dip = (d.X / dpi, d.Y / dpi);
        let glass = card::glass_for(theme, under);
        let theme_for_content = card::theme_for(theme, under);
        let light = theme_for_content.is_light;
        let live = !theme.high_contrast && !theme.transparency_off && super::live::enabled();
        let bitmap = gfx.draw(d.X as u32, d.Y as u32, |dc, ox, oy| unsafe {
            dc.SetTransform(&Matrix3x2 {
                M11: dpi,
                M22: dpi,
                M31: ox,
                M32: oy,
                ..Matrix3x2::identity()
            });
            if glass.is_some() {
                card::glass_edge(dc, light, dip.0, dip.1)?;
            } else {
                card::edge(dc, light, dip.0, dip.1)?;
            }
            edit_view::content(dc, gfx, theme_for_content, preview, size, dip)
        })?;
        let pad = (SHADOW_PAD * dpi).ceil();
        let shadow = |t| {
            gfx.draw(
                (d.X + 2. * pad) as u32,
                (d.Y + 2. * pad) as u32,
                |dc, ox, oy| unsafe {
                    dc.SetTransform(&Matrix3x2 {
                        M11: dpi,
                        M22: dpi,
                        M31: ox + pad,
                        M32: oy + pad,
                        ..Matrix3x2::identity()
                    });
                    crate::gfx::soft_shadows(
                        dc,
                        crate::companion::chat::paint::rect(0., 0., dip.0, dip.1),
                        card::RADIUS,
                        &card::lift_shadow(light, glass.is_some(), t),
                    )
                },
            )
        };
        let props = gfx.compositor.CreatePropertySet()?;
        props.InsertVector3(&HSTRING::from("hand"), pos(at))?;
        props.InsertVector3(&HSTRING::from("slack"), Vector3::zero())?;
        props.InsertVector3(&HSTRING::from("baseScale"), xyz(1., 1., 1.))?;
        props.InsertVector3(&HSTRING::from("targetPosition"), pos(at))?;
        props.InsertVector3(&HSTRING::from("targetSize"), xyz(d.X, d.Y, 0.))?;
        props.InsertScalar(&HSTRING::from("lift"), 0.)?;
        Ok(Self {
            bitmap,
            rest_shadow: shadow(0.)?,
            drag_shadow: shadow(1.)?,
            props,
            size: d,
            dpi,
            light,
            glass,
            live,
        })
    }

    pub fn bring_up(&self, c: &Compositor, origin: RECT, hand: RECT, offset: (f32, f32)) -> Result<()> {
        let hand_position = hand_position(hand, offset);
        self.props.StopAnimation(&HSTRING::from("hand"))?;
        self.props.InsertVector3(&HSTRING::from("hand"), hand_position)?;
        if motion::full() && pos(origin) != hand_position {
            self.props.InsertVector3(&HSTRING::from("slack"), pos(origin) - hand_position)?;
            vector_spring(c, &self.props, "slack", Vector3::zero(), LIFT_SLACK, LIFT_SLACK_MS)?;
        }
        lift(c, &self.props, true)
    }

    pub fn move_to(&self, c: &Compositor, r: RECT, offset: (f32, f32), resize: bool) -> Result<()> {
        self.props.InsertVector3(&HSTRING::from("hand"), hand_position(r, offset))?;
        if resize {
            self.resize(c, r, motion::SPRING_SNAP, TARGET_MS)?;
        }
        Ok(())
    }

    pub fn land(
        &self,
        c: &Compositor,
        at: RECT,
        offset: (f32, f32),
        to: RECT,
        velocity: (f32, f32),
        scale: Option<f32>,
    ) -> Result<()> {
        animate_slack(
            c, &self.props, pos(to) - hand_position(at, offset),
            velocity, scale.unwrap_or(self.dpi),
        )?;
        self.resize(c, to, STILL_LANDING, DROP_MS)?;
        lift(c, &self.props, false)
    }

    fn resize(&self, c: &Compositor, r: RECT, spring: motion::Spring, ms: i64) -> Result<()> {
        let d = dims(r);
        vector_spring(c, &self.props, "baseScale", xyz(d.X / self.size.X, d.Y / self.size.Y, 1.), spring, ms)
    }

    pub fn select(&self, c: &Compositor, r: RECT, animate: bool) -> Result<()> {
        let d = dims(r);
        let size = xyz(d.X, d.Y, 0.);
        if animate {
            vector_spring(
                c,
                &self.props,
                "targetPosition",
                pos(r),
                motion::SPRING_SNAP,
                TARGET_MS,
            )?;
            vector_spring(
                c,
                &self.props,
                "targetSize",
                size,
                motion::SPRING_SNAP,
                TARGET_MS,
            )
        } else {
            self.props
                .InsertVector3(&HSTRING::from("targetPosition"), pos(r))?;
            self.props.InsertVector3(&HSTRING::from("targetSize"), size)
        }
    }

    pub fn attach_card(
        &self,
        gfx: &Gfx,
        parent: &ContainerVisual,
        hwnd: Option<HWND>,
        m: Monitor,
    ) -> Result<CardVisual> {
        let c = &gfx.compositor;
        let mut visual = CardVisual {
            root: c.CreateContainerVisual()?,
            glass: None,
        };
        let root = &visual.root;
        root.SetSize(self.size)?;
        let center = (self.size.X / 2., self.size.Y / 2.);
        root.SetCenterPoint(xyz(center.0, center.1, 0.))?;
        // Keep scaling across displays anchored at the grabbed top left.
        // Lift itself grows around the card center.
        bind(
            c,
            root,
            "Offset",
            &format!(
                "p.hand + p.slack + (p.baseScale - Vector3(1, 1, 1)) * Vector3({:.2}, {:.2}, 0) - Vector3({}, {}, 0)",
                center.0, center.1, m.work.left, m.work.top,
            ),
            &self.props,
        )?;
        let grow = LIFT_SCALE - 1.;
        bind(
            c,
            root,
            "Scale",
            &format!("p.baseScale * Vector3(1+p.lift*{grow:.4}, 1+p.lift*{grow:.4}, 1)"),
            &self.props,
        )?;
        let pad = (SHADOW_PAD * self.dpi).ceil();
        let shadow_size = xy(self.size.X + 2. * pad, self.size.Y + 2. * pad);
        for (s, a) in [
            (&self.rest_shadow, "1-Clamp(p.lift,0,1)"),
            (&self.drag_shadow, "Clamp(p.lift,0,1)"),
        ] {
            let v = surface(c, s, shadow_size)?;
            v.SetOffset(xyz(-pad, -pad, 0.))?;
            bind(c, &v, "Opacity", a, &self.props)?;
            root.Children()?.InsertAtTop(&v)?;
        }
        let base = c.CreateSpriteVisual()?;
        base.SetSize(self.size)?;
        let radius = card::RADIUS * self.dpi;
        rounded(c, &base, self.size, radius)?;
        if let Some((brush, cover)) = self
            .live
            .then(|| super::live::drag_brush(c, hwnd, self.light))
            .flatten()
        {
            base.SetBrush(&brush)?;
            let tint = c.CreateSpriteVisual()?;
            tint.SetSize(self.size)?;
            rounded(c, &tint, self.size, radius)?;
            tint.SetBrush(&c.CreateColorBrushWithColor(color(cover))?)?;
            root.Children()?.InsertAtTop(&base)?;
            root.Children()?.InsertAtTop(&tint)?;
        } else {
            base.SetBrush(&c.CreateColorBrushWithColor(color(card::surface(self.light)))?)?;
            root.Children()?.InsertAtTop(&base)?;
            if let Some((glass, grey)) = self.glass.zip(backdrop::picture_on(m)) {
                let wallpaper = glass_surface(gfx, &grey, glass.tint)?;
                let brush = c.CreateSurfaceBrushWithSurface(&wallpaper.surface)?;
                visual.glass = Some((brush.clone(), wallpaper.clone()));
                brush.SetStretch(CompositionStretch::None)?;
                brush.SetHorizontalAlignmentRatio(0.)?;
                brush.SetVerticalAlignmentRatio(0.)?;
                bind_glass(c, &brush, &self.props, xy(center.0, center.1), &wallpaper)?;
                let sample = c.CreateSpriteVisual()?;
                sample.SetSize(self.size)?;
                rounded(c, &sample, self.size, radius)?;
                sample.SetBrush(&brush)?;
                root.Children()?.InsertAtTop(&sample)?;
            }
        }
        root.Children()?
            .InsertAtTop(&surface(c, &self.bitmap, self.size)?)?;
        parent.Children()?.InsertAtTop(root)?;
        Ok(visual)
    }

    pub fn attach_marks(
        &self,
        gfx: &Gfx,
        root: &ContainerVisual,
        m: Monitor,
        slots: &[RECT],
    ) -> Result<Marks> {
        let c = &gfx.compositor;
        let dots = c.CreateShapeVisual()?;
        dots.SetSize(dims(m.work))?;
        let tone = if self.light { 0 } else { 255 };
        let brush = c.CreateColorBrushWithColor(color(Rgba {
            r: tone,
            g: tone,
            b: tone,
            a: if self.light { 28 } else { 43 },
        }))?;
        let radius = 1.4 * m.scale();
        let disk = c.CreateEllipseGeometry()?;
        disk.SetRadius(xy(radius, radius))?;
        let shapes = dots.Shapes()?;
        for &slot in slots {
            let dot = c.CreateSpriteShapeWithGeometry(&disk)?;
            let p = centre(slot);
            dot.SetOffset(xy(p.X - m.work.left as f32, p.Y - m.work.top as f32))?;
            dot.SetFillBrush(&brush)?;
            shapes.Append(&dot)?;
        }
        dots.SetOpacity(0.)?;
        root.Children()?.InsertAtTop(&dots)?;

        let target = c.CreateSpriteVisual()?;
        let round = rounded(c, &target, self.size, card::RADIUS * m.scale())?;
        for v in [target.cast::<CompositionObject>()?, round.cast()?] {
            bind(
                c,
                &v,
                "Size",
                "Vector2(p.targetSize.X,p.targetSize.Y)",
                &self.props,
            )?;
        }
        bind(
            c,
            &target,
            "Offset",
            &format!(
                "p.targetPosition - Vector3({}, {}, 0)",
                m.work.left, m.work.top
            ),
            &self.props,
        )?;
        let fill = if self.light {
            Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 15,
            }
        } else {
            Rgba {
                r: 255,
                g: 255,
                b: 255,
                a: 20,
            }
        };
        target.SetBrush(&c.CreateColorBrushWithColor(color(fill))?)?;
        target.SetOpacity(0.)?;
        root.Children()?.InsertAtTop(&target)?;
        Ok(Marks { dots, target })
    }
}

pub(super) struct Marks {
    pub dots: ShapeVisual,
    pub target: SpriteVisual,
}

impl Drop for Marks {
    fn drop(&mut self) {
        if let Ok(shapes) = self.dots.Shapes() {
            let _ = shapes.Clear();
        }
        let _ = self.dots.Close();
        let _ = self.target.Close();
    }
}

pub(super) fn drop_ms() -> u32 {
    if motion::full() {
        crate::effects::ms(DROP_MS) as u32
    } else {
        motion::REDUCED_MS as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        desklayer::{
            graphite_snapshots as gs,
            grid::{Grid, Span},
            widget,
        },
        gfx::promo_kit as kit,
    };

    #[test]
    fn landing_keys_keep_direction_and_settle_exactly_on_both_axes() {
        let to = xyz(12., -12., 0.);
        let slow: Vec<_> = landing_keys(to, (60., -60.), 1., DROP_MS).collect();
        let quick: Vec<_> = landing_keys(to, (1200., -1200.), 1., DROP_MS).collect();
        assert_eq!(quick.len(), 61);
        assert_eq!(quick[0].position, Vector3::zero());
        assert_eq!(quick[0].start_weight, 1.);
        assert!(quick[1].position.X > slow[1].position.X);
        for key in &slow {
            assert!((0. ..=to.X).contains(&key.position.X));
            assert_eq!(key.position.Y, -key.position.X);
        }
        let peak = quick.iter().map(|key| key.position.X).fold(0., f32::max);
        assert!((to.X + 2. ..to.X + 5.).contains(&peak));
        let ending = quick.last().unwrap();
        assert_eq!(ending.position, to);
        assert_eq!(ending.start_weight, 0.);
        assert!((quick[59].position.X - to.X).abs() < 0.01);
    }

    #[test]
    fn landing_uses_the_running_lift_position_even_when_returning_against_the_hand() {
        let to = xyz(-90., 0., 0.);
        let start = xyz(14., 0., 0.);
        let keys: Vec<_> = landing_keys(to, (1200., 0.), 1., DROP_MS).collect();
        let pos = |k: &LandingKey| k.position + start * k.start_weight;
        assert_eq!(pos(&keys[0]), start);
        // With a large return distance the acceleration can reverse the
        // motion before the first 5 ms key. Check the analytic interpolation.
        let spring = motion::Spring { damping: STILL_LANDING.damping * 0.88, ..STILL_LANDING };
        assert!((pos(&keys[1]).X - spring.at(14., 1200., -90., 0.005).0).abs() < 0.001);
        assert_eq!(pos(keys.last().unwrap()), to);
    }

    #[test]
    fn repeated_drags_release_glass_surfaces_and_visuals() {
        let stage = kit::stage();
        let c = &stage.gfx.compositor;
        let m = gs::monitor(320, 240, 1.);
        let image = gs::wall(gs::Wall::Bright, 320, 240, 1.);
        backdrop::set_test(backdrop::grey_of(m.rect, gs::mica(&image)));
        let at = RECT {
            left: 32,
            top: 44,
            right: 232,
            bottom: 208,
        };
        let under = backdrop::under(at).unwrap();
        let theme = kit::theme(true);
        let mut retired = Vec::new();
        for _ in 0..8 {
            let scene = c.CreateContainerVisual().unwrap();
            let face = Face::new(
                &stage.gfx,
                &theme,
                &Preview::Note(String::new(), String::new()),
                Size::MEDIUM,
                at,
                1.,
                Some(&under),
            )
            .unwrap();
            let visual = face.attach_card(&stage.gfx, &scene, None, m).unwrap();
            let (brush, glass) = visual.glass.as_ref().unwrap();
            // WinRT Composition objects do not expose weak references.
            // Keep observer handles and count the ones still usable after Close.
            retired.push((visual.root.clone(), brush.clone(), glass.surface.clone()));
            scene.Children().unwrap().RemoveAll().unwrap();
            drop(visual);
            let _ = scene.Close();
        }
        GLASS_CACHE.with(|cache| assert_eq!(cache.borrow().len(), 1));
        assert!(retired
            .iter()
            .all(|(_, _, surface)| surface == &retired[0].2));
        let next = backdrop::grey_of(m.rect, gs::mica(&image)).unwrap();
        let new_generation = glass_surface(&stage.gfx, &next, under.glass.tint).unwrap();
        let mut tint = under.glass.tint;
        tint.a = tint.a.saturating_sub(1);
        let new_tint = glass_surface(&stage.gfx, &next, tint).unwrap();
        assert!(
            new_generation.surface != retired[0].2 && new_generation.surface != new_tint.surface
        );
        GLASS_CACHE.with(|cache| cache.borrow_mut().clear());
        backdrop::set_test(None);
        let open = retired
            .iter()
            .filter(|(visual, brush, surface)| {
                visual.Offset().is_ok() || brush.Offset().is_ok() || surface.Size().is_ok()
            })
            .count();
        assert_eq!(open, 0);
    }

    #[test]
    #[ignore = "offscreen rendering of the Composition drag visuals"]
    fn widget_motion_frames_pngs() {
        let out = kit::harmony_dir().join("drag-smooth");
        std::fs::create_dir_all(&out).unwrap();
        let mut table = String::from("ms,slow_drop_target12,fast_drop_target12,cancel_target_minus90\n");
        use std::fmt::Write as _;
        let slow = landing_keys(xyz(12., 0., 0.), (60., 0.), 1., DROP_MS);
        let fast = landing_keys(xyz(12., 0., 0.), (1200., 0.), 1., DROP_MS);
        let cancel = landing_keys(xyz(-90., 0., 0.), (1200., 0.), 1., DROP_MS);
        for ((slow, fast), cancel) in slow.zip(fast).zip(cancel) {
            writeln!(table, "{:.0},{:.4},{:.4},{:.4}", slow.progress * DROP_MS as f32,
                slow.position.X, fast.position.X, cancel.position.X).unwrap();
        }
        std::fs::write(out.join("landing.csv"), table).unwrap();
        let stage = kit::stage();
        let _lang = crate::i18n::test_lock("ko");
        for (dpi, height, wall, name) in [
            (96, 1440, gs::Wall::Bright, "100"),
            (144, 1600, gs::Wall::Night, "150"),
        ] {
            let d = dpi as f32 / 96.;
            let mut m = gs::monitor(2560, height, d);
            m.work.top = (30. * d).round() as i32;
            m.work.bottom -= (66. * d).round() as i32;
            let img = gs::wall(wall, 2560, height, d);
            backdrop::set_test(backdrop::grey_of(m.rect, gs::mica(&img)));
            let g = Grid::widget(&m, ((76. * d) as i32, (96. * d) as i32)).unwrap();
            let at = |c, r| g.rect(Span { c, r, w: 4, h: 2 }, 0);
            let origin = at(g.cols - 4, 0);
            let target = at(g.cols - 10, 3);
            let carried = RECT {
                left: target.left - 130,
                right: target.right - 130,
                top: target.top + 70,
                bottom: target.bottom + 70,
            };
            let theme = kit::theme(dpi == 96);
            let preview = Preview::Note(gs::MEMO.to_string(), "오후 2:10".into());
            let under = backdrop::under(origin).unwrap();
            let face = Face::new(
                &stage.gfx,
                &theme,
                &preview,
                Size::MEDIUM,
                origin,
                d,
                Some(&under),
            )
            .unwrap();
            let c = &stage.gfx.compositor;
            let root = c.CreateContainerVisual().unwrap();
            root.SetSize(dims(m.rect)).unwrap();
            let wallpaper = stage
                .gfx
                .surface_from_bgra(img.width, img.height, &img.pixels)
                .unwrap();
            let bg = surface(c, &wallpaper, dims(m.rect)).unwrap();
            root.Children().unwrap().InsertAtBottom(&bg).unwrap();

            let work = c.CreateContainerVisual().unwrap();
            work.SetOffset(pos(m.work)).unwrap();
            work.SetSize(dims(m.work)).unwrap();
            work.SetClip(&c.CreateInsetClip().unwrap()).unwrap();
            root.Children().unwrap().InsertAtTop(&work).unwrap();
            let marks = face
                .attach_marks(&stage.gfx, &work, m, &widget::free_slots(&g, 0, &[]))
                .unwrap();
            let _card = face.attach_card(&stage.gfx, &work, None, m).unwrap();
            face.select(c, target, false).unwrap();

            let picture = |suffix, at, lift, dots| {
                face.props
                    .InsertVector3(&HSTRING::from("hand"), pos(at))
                    .unwrap();
                face.props
                    .InsertScalar(&HSTRING::from("lift"), lift)
                    .unwrap();
                marks.dots.SetOpacity(dots).unwrap();
                marks.target.SetOpacity(dots).unwrap();
                let px =
                    unsafe { crate::popup::chrome_harmony::read_back(&root, 2560, height as i32) }
                        .unwrap();
                kit::save_png(
                    &px,
                    2560,
                    height,
                    &out.join(format!("drag-{name}-{suffix}.png")),
                );
                px
            };
            picture("lift-60ms", origin, 1. - LIFT.left(0.060), 0.5);
            let moved = picture("held", carried, 1., 1.);
            let sample = centre(carried);
            let sample_y = sample.Y as i32 + 50;
            let sample_x = sample.X as i32 + 50;
            let i = ((sample_y * 2560 + sample_x) * 4) as usize;
            let mut expected = under.grey.img.pixels[i..i + 4].to_vec();
            card::tinted(&mut expected, under.glass.tint);
            for channel in 0..3 {
                assert!(
                    moved[i + channel].abs_diff(expected[channel]) < 6,
                    "wallpaper sample at {sample_x},{sample_y}: {:?} != {expected:?}",
                    &moved[i..i + 4]
                );
            }
            let progress = 1. - STILL_LANDING.left(0.13);
            picture(
                "drop-130ms",
                super::super::edit::between(carried, target, progress),
                1. - progress,
                0.,
            );
            picture("drop-300ms", target, 0., 0.);
            // Exercise the same WinRT animations used by both drag paths.
            face.props
                .InsertVector3(&HSTRING::from("hand"), pos(origin))
                .unwrap();
            face.bring_up(c, origin, origin, (0., 0.)).unwrap();
            face.move_to(c, carried, (0., 0.), false).unwrap();
            face.move_to(c, target, (0., 0.), false).unwrap();
            face.land(c, target, (0., 0.), target, (1200., 0.), Some(d)).unwrap();
            let drop_frame =
                unsafe { crate::popup::chrome_harmony::read_back(&root, 2560, height as i32) }
                    .unwrap();
            let sample = centre(target);
            let i = (((sample.Y as i32 + 50) * 2560 + sample.X as i32 + 50) * 4) as usize;
            let mut expected = under.grey.img.pixels[i..i + 4].to_vec();
            card::tinted(&mut expected, under.glass.tint);
            for channel in 0..3 {
                assert!(drop_frame[i + channel].abs_diff(expected[channel]) < 6);
            }
            backdrop::set_test(None);
        }
    }
}
