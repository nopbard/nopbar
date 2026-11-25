//! Live wallpaper glass. Only Composition passes opt into transparent cards.
//! No readback/capture or per-frame wallpaper polling: DWM owns the backdrop.
use std::cell::RefCell;
#[cfg(not(test))]
use std::sync::atomic::{AtomicU8, Ordering};
use windows::{
    core::Result,
    Win32::{
        Foundation::HWND,
        Graphics::{
            Direct2D::ID2D1DeviceContext,
            Dwm::{DwmSetWindowAttribute, DWMWA_USE_HOSTBACKDROPBRUSH},
        },
    },
    UI::Composition::{Compositor, ContainerVisual, SpriteVisual, CompositionRoundedRectangleGeometry},
};
use windows_numerics::{Matrix3x2, Vector2, Vector3};

// 0 unknown, 1 static, 2 live, 3 live with transparency/power restriction.
#[cfg(not(test))]
static MODE: AtomicU8 = AtomicU8::new(0);

pub fn refresh() {
    #[cfg(not(test))]
    {
        let live = crate::wallpaper::live_wallpaper();
        let restricted =
            !crate::syslook::current().transparency || crate::sysstat::read_power().saver;
        let next = if !live {
            1
        } else if restricted {
            3
        } else {
            2
        };
        if MODE.swap(next, Ordering::Relaxed) != next {
            super::backdrop::changed();
        }
    }
}

fn mode() -> u8 {
    #[cfg(test)]
    {
        TEST_MODE.with(|m| *m.borrow())
    }
    #[cfg(not(test))]
    {
        if MODE.load(Ordering::Relaxed) == 0 {
            refresh();
        }
        MODE.load(Ordering::Relaxed)
    }
}

pub fn enabled() -> bool {
    mode() >= 2
}

pub(super) fn drag_brush(
    c: &Compositor,
    hwnd: Option<HWND>,
    light: bool,
) -> Option<(windows::UI::Composition::CompositionBrush, crate::theme::Rgba)> {
    let mode = mode();
    if mode < 2 || !enable_host(hwnd) { return None; }
    let mut tint = super::card::tint(light);
    tint.a = (cover(light, mode == 3) * 255.).round() as u8;
    Some((crate::glass::host_backdrop(c).ok()?, tint))
}

fn enable_host(hwnd: Option<HWND>) -> bool {
    hwnd.is_none_or(|h| unsafe {
        let yes = windows::core::BOOL(1);
        DwmSetWindowAttribute(h, DWMWA_USE_HOSTBACKDROPBRUSH, &yes as *const _ as _, std::mem::size_of_val(&yes) as u32).is_ok()
    })
}
pub fn cover(light: bool, restricted: bool) -> f32 {
    match (light, restricted) {
        (false, false) => 0.40,
        (true, false) => 0.58,
        (false, true) => 0.55,
        (true, true) => 0.72,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Area {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    rx: f32,
    ry: f32,
}
struct Pass {
    offset: (f32, f32),
    areas: Vec<Area>,
    restricted: bool,
}
thread_local! {
    static PASS: RefCell<Option<Pass>> = const { RefCell::new(None) };
    #[cfg(test)]
    static TEST_MODE: RefCell<u8> = const { RefCell::new(1) };
}
#[cfg(test)]
pub struct TestMode(u8);
#[cfg(test)]
impl TestMode {
    pub fn new(restricted: bool) -> Self {
        Self(TEST_MODE.with(|m| m.replace(if restricted { 3 } else { 2 })))
    }
}
#[cfg(test)]
impl Drop for TestMode {
    fn drop(&mut self) {
        TEST_MODE.with(|m| {
            m.replace(self.0);
        });
    }
}

struct Scope(Option<Pass>);
impl Drop for Scope {
    fn drop(&mut self) {
        PASS.with(|p| {
            p.replace(self.0.take());
        });
    }
}

pub fn drawing() -> bool {
    PASS.with(|p| p.borrow().is_some())
}
/// Records the painter's actual transform, including preview scaling and D2D's
/// atlas offset. The card replaces the previous opaque fill in this area.
pub unsafe fn area(dc: &ID2D1DeviceContext, w: f32, h: f32, light: bool) -> Option<f32> {
    PASS.with(|p| {
        let mut p = p.borrow_mut();
        let p = p.as_mut()?;
        let mut m = Matrix3x2::identity();
        dc.GetTransform(&mut m);
        p.areas.push(Area {
            x: m.M31 - p.offset.0,
            y: m.M32 - p.offset.1,
            w: w * m.M11,
            h: h * m.M22,
            rx: super::card::RADIUS * m.M11,
            ry: super::card::RADIUS * m.M22,
        });
        Some(cover(light, p.restricted))
    })
}

/// One layer beneath a drawing surface; the rounded host brushes only cover
/// cards actually painted in that surface. Unchanged geometry is reused.
pub struct Layer {
    root: ContainerVisual,
    compositor: Compositor,
    areas: RefCell<Vec<Area>>,
    nodes: RefCell<Vec<(SpriteVisual, CompositionRoundedRectangleGeometry)>>,
    supported: bool,
}
impl Layer {
    pub fn new(c: &Compositor, parent: &ContainerVisual, hwnd: Option<HWND>) -> Result<Self> {
        let supported = enable_host(hwnd);
        let root = c.CreateContainerVisual()?;
        parent.Children()?.InsertAtBottom(&root)?;
        Ok(Self {
            root,
            compositor: c.clone(),
            areas: RefCell::new(Vec::new()),
            nodes: RefCell::new(Vec::new()),
            supported,
        })
    }
    pub fn attach(&self, parent: &ContainerVisual) -> Result<()> {
        parent.Children()?.InsertAtBottom(&self.root)
    }
    pub fn draw(&self, offset: (f32, f32), paint: impl FnOnce() -> Result<()>) -> Result<()> {
        let pass = (self.supported && enabled()).then(|| Pass {
            offset,
            areas: Vec::new(),
            restricted: mode() == 3,
        });
        let scope = Scope(PASS.with(|p| p.replace(pass)));
        let result = paint();
        let areas = PASS
            .with(|p| {
                p.borrow_mut()
                    .as_mut()
                    .map(|p| std::mem::take(&mut p.areas))
            })
            .unwrap_or_default();
        drop(scope);
        result?;
        if *self.areas.borrow() != areas {
            let mut nodes = self.nodes.borrow_mut();
            for (i, a) in areas.iter().enumerate() {
                let c = &self.compositor;
                if i == nodes.len() {
                    let sprite = c.CreateSpriteVisual()?;
                    let round = c.CreateRoundedRectangleGeometry()?;
                    sprite.SetClip(&c.CreateGeometricClipWithGeometry(&round)?)?;
                    sprite.SetBrush(&crate::glass::host_backdrop(c)?)?;
                    // Configure before insertion; all changes precede surface EndDraw.
                    sprite.SetIsVisible(false)?;
                    self.root.Children()?.InsertAtTop(&sprite)?;
                    nodes.push((sprite, round));
                }
                let (sprite, round) = &nodes[i];
                sprite.SetOffset(Vector3 {
                    X: a.x,
                    Y: a.y,
                    Z: 0.,
                })?;
                sprite.SetSize(Vector2 { X: a.w, Y: a.h })?;
                round.SetSize(Vector2 { X: a.w, Y: a.h })?;
                round.SetCornerRadius(Vector2 { X: a.rx, Y: a.ry })?;
                sprite.SetIsVisible(true)?;
            }
            for (sprite, _) in &nodes[areas.len()..] {
                sprite.SetIsVisible(false)?;
            }
            *self.areas.borrow_mut() = areas;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn review_moving_backdrop_keeps_visual_brush_and_clip() {
        use windows::core::Interface;
        let stage = crate::gfx::promo_kit::stage();
        let parent = stage.gfx.compositor.CreateContainerVisual().unwrap();
        let layer = Layer::new(&stage.gfx.compositor, &parent, None).unwrap();
        let _ground = crate::glass::StandIn::new(crate::theme::Rgba { r: 120, g: 120, b: 120, a: 255 });
        let _mode = TestMode::new(false);
        let draw = |x| {
            super::super::surface::render(160, 120, 1., |dc, _| {
                layer.draw((0., 0.), || unsafe {
                    dc.SetTransform(&Matrix3x2 { M31: x, ..Matrix3x2::identity() });
                    super::super::card::body(dc, &crate::theme::Theme::graphite(), 100. + x, 60., None)
                })
            }).unwrap();
        };
        draw(0.);
        let first = layer.root.Children().unwrap().First().unwrap().Current().unwrap();
        let clip = first.Clip().unwrap();
        let brush = first.cast::<SpriteVisual>().unwrap().Brush().unwrap();
        for x in [1., 25., 80.] {
            draw(x);
            let next = layer.root.Children().unwrap().First().unwrap().Current().unwrap();
            assert_eq!(first, next);
            assert_eq!(clip, next.Clip().unwrap());
            assert_eq!(brush, next.cast::<SpriteVisual>().unwrap().Brush().unwrap());
            assert_eq!(next.Size().unwrap().X, 100. + x);
        }
    }
    #[test]
    fn fixed_cover_for_unknown_live_background() {
        assert_eq!((cover(false, false), cover(true, false)), (0.40, 0.58));
        assert_eq!((cover(false, true), cover(true, true)), (0.55, 0.72));
    }

    #[test]
    fn transformed_cards_and_static_transition() {
        let stage = crate::gfx::promo_kit::stage();
        let parent = stage.gfx.compositor.CreateContainerVisual().unwrap();
        let layer = Layer::new(&stage.gfx.compositor, &parent, None).unwrap();
        let _ground = crate::glass::StandIn::new(crate::theme::Rgba {
            r: 120,
            g: 120,
            b: 120,
            a: 255,
        });
        {
            let _mode = TestMode::new(false);
            super::super::surface::render(160, 120, 1., |dc, _| {
                layer.draw((3., 7.), || unsafe {
                    dc.SetTransform(&Matrix3x2 {
                        M11: 0.5,
                        M22: 1.5,
                        M31: 23.,
                        M32: 37.,
                        ..Matrix3x2::identity()
                    });
                    super::super::card::body(dc, &crate::theme::Theme::graphite(), 200., 60., None)
                })
            })
            .unwrap();
            assert_eq!(
                layer.areas.borrow()[0],
                Area {
                    x: 20.,
                    y: 30.,
                    w: 100.,
                    h: 90.,
                    rx: 11.,
                    ry: 33.
                }
            );
            assert!(!drawing());
            assert!(super::super::backdrop::under(Default::default()).is_none());
        }
        layer.draw((0., 0.), || Ok(())).unwrap();
        assert!(layer.areas.borrow().is_empty());
    }

    #[test]
    #[ignore = "offscreen Composition PNG inspection"]
    fn harmony_live_cards() {
        use crate::{
            gfx::promo_kit as kit,
            theme::{Rgba, Theme},
        };
        let stage = kit::stage();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../target/ui-snapshots/live-glass");
        for light in [false, true] {
            for restricted in [false, true] {
                for dpi in [1., 2.] {
                    let _mode = TestMode::new(restricted);
                    let _ground = crate::glass::StandIn::new(Rgba {
                        r: 100,
                        g: 140,
                        b: 180,
                        a: 255,
                    });
                    let mut theme = Theme::graphite();
                    theme.is_light = light;
                    let (w, h) = ((660. * dpi) as i32, (260. * dpi) as i32);
                    let glass =
                        super::super::surface::Glass::new(None, stage.gfx.clone(), (w, h), dpi)
                            .unwrap();
                    glass
                        .draw(|dc, gfx| unsafe {
                            // Opaque gallery underneath: live card interiors must replace it.
                            let bg = dc.CreateSolidColorBrush(
                                &super::super::card::rgba(super::super::card::surface(light)),
                                None,
                            )?;
                            dc.FillRectangle(
                                &crate::companion::chat::paint::rect(0., 0., 660., 260.),
                                &bg,
                            );
                            let mut base = Matrix3x2::identity();
                            dc.GetTransform(&mut base);
                            for (i, scale) in [1., 0.8, 1.03].into_iter().enumerate() {
                                dc.SetTransform(
                                    &(crate::companion::render::scale(scale, scale)
                                        * crate::companion::render::translate(
                                            20. + i as f32 * 215.,
                                            28.,
                                        )
                                        * base),
                                );
                                super::super::card::plate(dc, &theme, 190., 190., i == 2, None)?;
                                super::super::card::line(
                                    dc,
                                    gfx,
                                    ["위젯", "미리보기", "노움 카드"][i],
                                    16.,
                                    16.,
                                    super::super::card::Line::new(16., 600, 24.),
                                    super::super::card::ink(&theme, 1),
                                    160.,
                                )?;
                                super::super::card::line(
                                    dc,
                                    gfx,
                                    "실시간 유리",
                                    16.,
                                    65.,
                                    super::super::card::Line::new(14., 400, 22.),
                                    super::super::card::ink(&theme, 2),
                                    160.,
                                )?;
                            }
                            Ok(())
                        })
                        .unwrap();
                    let pixels =
                        unsafe { crate::popup::chrome_harmony::read_back(&glass.root, w, h) }
                            .unwrap();
                    let p = ((130. * dpi) as usize * w as usize + (70. * dpi) as usize) * 4;
                    let expected = super::super::card::glass_colour(
                        light,
                        [100., 140., 180.],
                        cover(light, restricted),
                    );
                    for (actual, expected) in pixels[p..p + 3].iter().rev().zip(expected) {
                        assert!((*actual as f32 - expected).abs() < 3.);
                    }
                    kit::save_png(
                        &pixels,
                        w as u32,
                        h as u32,
                        &dir.join(format!(
                            "live-{light}-{restricted}-{}.png",
                            (dpi * 100.) as u32
                        )),
                    );
                }
            }
        }
    }
}
