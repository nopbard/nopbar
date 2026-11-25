//! The hover name label (harmony v2.1, "이름표"): a small tag beside the
//! hovered icon: material M4 (the tips' tint, no backdrop of its own: no
//! glass on glass beside the dock's), a tooltip's corners (8), the rim and
//! the contact shadow, elevation E1. It waits TIP_DELAY_MS before the
//! first show, then follows the pointer from icon to icon at once (no new
//! wait, no new fade) until the pointer leaves the dock; back within
//! TIP_WARM_MS of a label closing, the next one shows without the wait.
//! It comes in over FADE_IN_MS and goes over FADE_OUT_MS, as a whole (a
//! tip is small and brief: its light does not come up apart, 7.2 rule 2).
//! While a menu of the dock's is up there is no label (7.4).

use super::*;
use windows::UI::Composition::CompositionSpriteShape;

/// Wait before the first label after the pointer arrives.
pub(super) const LABEL_DELAY_MS: u32 = crate::motion::TIP_DELAY_MS as u32;
/// One-shot: the label wait is over.
pub(super) const LABEL_TIMER: usize = 12;
/// Gap between the icon's far edge and the label (a surface and what
/// opened it).
pub(super) const LABEL_GAP: f32 = crate::theme::metrics::size::EDGE_GAP;

/// The visuals of the label: a container (moved / faded as a whole)
/// holding its E1 and contact shadow and, in a rounded clip, the backdrop
/// (always hidden now), the tint, the text and an edge: the rim, or high
/// contrast's outline.
pub(super) struct LabelParts {
    pub shadow: SpriteVisual,
    /// Shadow pictures by size (px), a few at a time.
    pub shadows: HashMap<(u32, u32), CompositionSurfaceBrush>,
    pub pill: ContainerVisual,
    pub back: SpriteVisual,
    pub tint: SpriteVisual,
    pub text: SpriteVisual,
    pub clip: CompositionRoundedRectangleGeometry,
    pub border: CompositionRoundedRectangleGeometry,
    pub border_shape: CompositionSpriteShape,
    pub border_v: windows::UI::Composition::ShapeVisual,
    /// The label is up (or fading in); hidden otherwise.
    pub shown: bool,
    /// When the last label went (the warm state, TIP_WARM_MS).
    pub closed: Option<Instant>,
}

/// Pure: a label closed `since` ago leaves the next one without its wait
/// (6.3: within TIP_WARM_MS).
pub(super) fn warm(since: Option<std::time::Duration>) -> bool {
    since.is_some_and(|d| d.as_millis() < crate::motion::TIP_WARM_MS as u128)
}

/// Pure: the label's edge stroke for a `w` x `h` DIP pill with corners
/// `r`: (inset, size, corner, thickness), DIP. The rim is `rim` DIP just
/// inside the edge (Theme::rim_width: one device pixel, 2.2, spike S7;
/// Flat v2's 1 DIP); high contrast's outline 1 DIP.
pub(super) fn edge_stroke(w: f32, h: f32, r: f32, rim: f32, high_contrast: bool) -> (f32, (f32, f32), f32, f32) {
    let t = if high_contrast { 1.0 } else { rim };
    (t / 2.0, ((w - t).max(0.0), (h - t).max(0.0)), (r - t / 2.0).max(0.0), t)
}

/// The label's Opacity as crate::motion::Prop, for `cut`: assigning stops
/// what animates it first (its fade in or out).
struct LabelOpacity<'a>(&'a ContainerVisual);

impl crate::motion::Prop for LabelOpacity<'_> {
    type V = f32;

    fn set(&self, v: f32) -> Result<()> {
        let _ = self.0.StopAnimation(&HSTRING::from("Opacity"));
        self.0.SetOpacity(v)
    }

    /// (`cut` never animates: at once.)
    fn start(&self, _from: crate::motion::Start<f32>, to: f32) -> Result<()> {
        self.set(to)
    }
}

/// Pure but for `p` (the label's Opacity): the label goes at once (a menu
/// opens, 7.4). Whatever its Opacity is doing (coming in, already going)
/// stops at 0; one that was up or coming in counts as closed `now` (the
/// warm state: the next label within TIP_WARM_MS skips the wait), one
/// already going keeps the time it started going. `shown` is false after,
/// so the fade-out (hide_label) has nothing left to do.
pub(super) fn cut<P: crate::motion::Prop<V = f32>>(p: &P, shown: &mut bool, closed: &mut Option<Instant>, now: Instant) {
    if std::mem::take(shown) {
        *closed = Some(now);
    }
    let _ = p.set(0.0);
}

/// Builds the (hidden) label.
pub(super) fn make_label(c: &Compositor) -> Result<(ContainerVisual, LabelParts)> {
    let root = c.CreateContainerVisual()?;
    let shadow = c.CreateSpriteVisual()?;
    let pill = c.CreateContainerVisual()?;
    let clip = c.CreateRoundedRectangleGeometry()?;
    pill.SetClip(&c.CreateGeometricClipWithGeometry(&clip)?)?;
    root.Children()?.InsertAtTop(&shadow)?;
    root.Children()?.InsertAtTop(&pill)?;
    // (The host backdrop stays hidden: the label is solid.)
    let back = c.CreateSpriteVisual()?;
    back.SetBrush(&c.CreateHostBackdropBrush()?)?;
    let tint = c.CreateSpriteVisual()?;
    let text = c.CreateSpriteVisual()?;
    let border = c.CreateRoundedRectangleGeometry()?;
    border.SetOffset(Vector2 { X: 0.5, Y: 0.5 })?;
    let border_shape = c.CreateSpriteShapeWithGeometry(&border)?;
    border_shape.SetStrokeThickness(1.0)?;
    let border_v = c.CreateShapeVisual()?;
    border_v.Shapes()?.Append(&border_shape)?;
    back.SetOpacity(0.0)?;
    pill.Children()?.InsertAtTop(&back)?;
    pill.Children()?.InsertAtTop(&tint)?;
    pill.Children()?.InsertAtTop(&text)?;
    pill.Children()?.InsertAtTop(&border_v)?;
    root.SetOpacity(0.0)?;
    Ok((root, LabelParts { shadow, shadows: HashMap::new(), pill, back, tint, text, clip, border, border_shape, border_v, shown: false, closed: None }))
}

impl Dock {
    /// Colors the label for the current theme: material M4 (label_colors:
    /// opaque on Flat, at its own opacity with Glass); its edge is the rim
    /// (theme::rim), or the text color's outline in high contrast.
    pub(super) fn style_label(&mut self) {
        let c = self.gfx.compositor.clone();
        let (bg, fg) = self.theme.label_colors();
        let p = &mut self.label_parts;
        let _ = p.back.SetOpacity(0.0);
        // Flat v2: its fall from top to bottom.
        if let Ok(b) = crate::glass::tint_brush(&c, &self.theme, crate::theme::Material::Tip, bg) {
            let _ = p.tint.SetBrush(&b);
        }
        if self.theme.high_contrast {
            if let Ok(b) = c.CreateColorBrushWithColor(rgba(fg)) {
                let _ = p.border_shape.SetStrokeBrush(&b);
            }
        } else {
            paint_highlight(&c, &p.border_shape, &self.theme, crate::theme::Material::Tip);
        }
        // New colors or scale: the shadows are drawn again.
        p.shadows.clear();
    }

    /// Shows or hides the label as a whole: in over FADE_IN_MS, out over
    /// FADE_OUT_MS (show()); at once with the "Label fade" effect off. Only
    /// a change does anything.
    fn set_label(&mut self, on: bool) {
        if std::mem::replace(&mut self.label_parts.shown, on) == on {
            return;
        }
        crate::companion::refresh();
        if on { unsafe {
            // The label shares the dock HWND. Raise its owner once on open;
            // the companion keeps moving without fighting this z-order.
            let _=windows::Win32::UI::WindowsAndMessaging::SetWindowPos(
                self.hwnd,Some(windows::Win32::UI::WindowsAndMessaging::HWND_TOPMOST),0,0,0,0,
                windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE
                |windows::Win32::UI::WindowsAndMessaging::SWP_NOSIZE
                |windows::Win32::UI::WindowsAndMessaging::SWP_NOACTIVATE);
        }}
        if !on {
            self.label_parts.closed = Some(Instant::now());
        }
        if crate::effects::items().label_fade {
            show(&self.gfx.compositor, &self.label, on);
        } else {
            let _ = self.label.StopAnimation(&HSTRING::from("Opacity"));
            let _ = self.label.SetOpacity(if on { 1.0 } else { 0.0 });
        }
    }

    /// Takes the label away (it fades out; the wait is not reset): the
    /// pointer left it, previews opened, the dock went away.
    pub(super) fn hide_label(&mut self) {
        self.set_label(false);
    }

    /// Takes the label away at once, not over its fade: a menu opens (7.4:
    /// the label goes in the frame the menu opens). A fade under way, in or
    /// out, stops at 0 (see `cut`).
    pub(super) fn cut_label(&mut self) {
        let p = &mut self.label_parts;
        cut(&LabelOpacity(&self.label), &mut p.shown, &mut p.closed, Instant::now());
        crate::companion::refresh();
    }

    /// The label's shadows for a w x h DIP pill: E1 and, under its edge,
    /// the contact shadow (cached by pixel size; None with neither: high
    /// contrast).
    fn label_shadow(&mut self, w: f32, h: f32) -> Option<CompositionSurfaceBrush> {
        let layers = if crate::effects::items().shadows { self.theme.elevation(crate::theme::Elevation::E1) } else { Vec::new() };
        let contact = self.theme.contact();
        if layers.is_empty() && contact.a == 0 {
            return None;
        }
        let s = self.scale;
        let key = ((w * s).round() as u32, (h * s).round() as u32);
        if let Some(b) = self.label_parts.shadows.get(&key) {
            return Some(b.clone());
        }
        let m = LABEL_SHADOW_M * s;
        let (pw, ph) = ((w * s + m * 2.0).ceil(), (h * s + m * 2.0).ceil());
        let radius = label_radius(&self.theme, h) * s;
        let layers: Vec<_> = layers.into_iter().map(|(x, y, b, col)| (x * s, y * s, b * s, col)).collect();
        let surface = self
            .gfx
            .draw(pw as u32, ph as u32, |dc, ox, oy| unsafe {
                use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
                let r = D2D_RECT_F { left: ox + m, top: oy + m, right: ox + m + w * s, bottom: oy + m + h * s };
                crate::gfx::soft_shadows(dc, r, radius, &layers)?;
                if contact.a > 0 {
                    // The contact shadow: CONTACT_W past the edge, no blur.
                    let k = crate::theme::CONTACT_W * s;
                    let ring = windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F { left: r.left - k / 2.0, top: r.top - k / 2.0, right: r.right + k / 2.0, bottom: r.bottom + k / 2.0 },
                        radiusX: radius + k / 2.0,
                        radiusY: radius + k / 2.0,
                    };
                    let b = dc.CreateSolidColorBrush(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: contact.a as f32 / 255.0 }, None)?;
                    dc.DrawRoundedRectangle(&ring, &b, k, None);
                }
                Ok(())
            })
            .ok()?;
        let brush = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface).ok()?;
        let _ = brush.SetStretch(CompositionStretch::Fill);
        if self.label_parts.shadows.len() > 16 {
            self.label_parts.shadows.clear();
        }
        self.label_parts.shadows.insert(key, brush.clone());
        Some(brush)
    }

    /// The pointer left the dock: the next label waits again (unless it
    /// is back while still warm).
    pub(super) fn reset_label(&mut self) {
        self.label_armed = false;
        if std::mem::take(&mut self.label_waiting) {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), LABEL_TIMER);
            }
        }
        self.hide_label();
    }

    /// LABEL_TIMER fired: labels show from now on (until the pointer leaves).
    pub(super) fn label_wait_over(&mut self) {
        self.label_waiting = false;
        if self.mouse.is_some() {
            self.label_armed = true;
            self.layout_label();
        }
    }

    pub(super) fn layout_label(&mut self) {
        // A pinned icon held off the dock long enough: "Remove" beyond it
        // (no wait, no name).
        if let Some((u, top)) = self.remove_hint_at() {
            match self.label_brush(crate::i18n::t("dock.drag.remove")) {
                Some((brush, w, h)) => {
                    let (x, y) = self.frame.label_at(u, top, w, h, LABEL_GAP);
                    self.show_label(&brush, w, h, x, y);
                }
                None => self.hide_label(),
            }
            return;
        }
        // No name label while previews are open (they show titles), while
        // dragging, or while a menu is up (7.4: it goes in the frame the
        // menu opens and stays away while the menu is there).
        let quiet = self.hidden || preview::is_open() || self.dragging() || crate::popup::is_tracking();
        let hovered = if quiet { None } else { self.mouse.and_then(|(mu, _)| self.hit(mu)) };
        let Some(i) = hovered.filter(|&i| !self.items[i].name.is_empty()) else {
            self.hide_label();
            return;
        };
        if !self.label_armed && warm(self.label_parts.closed.map(|t| t.elapsed())) {
            // Back while still warm: no wait.
            self.label_armed = true;
            if std::mem::take(&mut self.label_waiting) {
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), LABEL_TIMER);
                }
            }
        }
        if !self.label_armed {
            // First icon since the pointer arrived: wait, then show.
            if !self.label_waiting {
                self.label_waiting = true;
                unsafe {
                    SetTimer(Some(self.hwnd), LABEL_TIMER, LABEL_DELAY_MS, None);
                }
            }
            self.hide_label();
            return;
        }
        // A wheel just changed this app's volume: the level, for a moment.
        let vol = self.volume_label(i);
        let name = match vol {
            Some((p, m)) => format!("{}{} {p} {m}", sound::VOLUME_KEY, crate::i18n::current()),
            None if self.items[i].hung => crate::i18n::tf("dock.hung.label", &[("name", &self.items[i].name)]),
            None => self.items[i].name.clone(),
        };
        let made = match vol {
            Some((p, m)) => self.volume_brush(&name, p, m),
            None => self.label_brush(&name),
        };
        let Some((brush, w, h)) = made else { return };
        let it = &self.items[i];
        // Follows the icon as it magnifies (its far edge moves out).
        let icon_top = self.bar_rect.y + PAD_CROSS + self.look.icon - it.w;
        let (x, y) = self.frame.label_at(it.dx + it.w / 2.0, icon_top, w, h, LABEL_GAP);
        self.show_label(&brush, w, h, x, y);
    }

    /// The text of a label (cached), with its size in DIP.
    fn label_brush(&mut self, name: &str) -> Option<(CompositionSurfaceBrush, f32, f32)> {
        if !self.labels.contains_key(name) {
            // Text only: the pill (blur, tint, border) is made of visuals.
            let (bg, fg) = self.theme.label_colors();
            let clear = crate::theme::Rgba { a: 0, ..bg };
            if let Ok((surface, w, h)) = self.gfx.label(name, self.scale, clear, fg, self.theme.radii().tip) {
                if let Ok(brush) = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface) {
                    let _ = brush.SetStretch(CompositionStretch::Fill);
                    // A bounded cache: names come and go with apps.
                    if self.labels.len() > 64 {
                        self.labels.clear();
                    }
                    self.labels.insert(name.to_string(), (brush, w, h));
                }
            }
        }
        self.labels.get(name).cloned()
    }

    /// A level label (sound.rs draws it), cached like names under `key`.
    fn volume_brush(&mut self, key: &str, p: u8, muted: bool) -> Option<(CompositionSurfaceBrush, f32, f32)> {
        if !self.labels.contains_key(key) {
            let (surface, w, h) = self.volume_surface(p, muted)?;
            let brush = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface).ok()?;
            let _ = brush.SetStretch(CompositionStretch::Fill);
            if self.labels.len() > 64 {
                self.labels.clear();
            }
            self.labels.insert(key.to_string(), (brush, w, h));
        }
        self.labels.get(key).cloned()
    }

    /// How far (DIP, across the dock) from the window's far side a dragged
    /// icon must stay for "Remove" to fit beyond it.
    pub(super) fn remove_label_room(&mut self) -> f32 {
        let Some((_, w, h)) = self.label_brush(crate::i18n::t("dock.drag.remove")) else { return 0.0 };
        (if self.frame.vertical() { w } else { h }) + LABEL_GAP + 2.0
    }

    /// Shows the pill with `brush` (w x h) at window DIP (x, y).
    fn show_label(&mut self, brush: &CompositionSurfaceBrush, w: f32, h: f32, x: f32, y: f32) {
        let size = Vector2 { X: w, Y: h };
        let shadow = self.label_shadow(w, h);
        let p = &self.label_parts;
        let _ = p.text.SetBrush(brush);
        for v in [&p.back, &p.tint, &p.text] {
            let _ = v.SetSize(size);
        }
        let _ = p.border_v.SetSize(size);
        let _ = p.pill.SetSize(size);
        match shadow {
            Some(b) => {
                let m = LABEL_SHADOW_M;
                let _ = p.shadow.SetBrush(&b);
                let _ = p.shadow.SetOffset(Vector3 { X: -m, Y: -m, Z: 0.0 });
                let _ = p.shadow.SetSize(Vector2 { X: w + m * 2.0, Y: h + m * 2.0 });
                let _ = p.shadow.SetOpacity(1.0);
            }
            None => {
                let _ = p.shadow.SetOpacity(0.0);
            }
        }
        // A tooltip's corners (radii.tip, 8), not a capsule.
        let r = label_radius(&self.theme, h);
        let _ = p.clip.SetSize(size);
        let _ = p.clip.SetCornerRadius(Vector2 { X: r, Y: r });
        // Its edge: the rim, one device pixel, or high contrast's outline.
        let rim = self.theme.rim_width(crate::theme::Material::Tip, self.scale);
        let (inset, (ew, eh), er, thick) = edge_stroke(w, h, r, rim, self.theme.high_contrast);
        let _ = p.border.SetOffset(Vector2 { X: inset, Y: inset });
        let _ = p.border.SetSize(Vector2 { X: ew, Y: eh });
        let _ = p.border.SetCornerRadius(Vector2 { X: er, Y: er });
        let _ = p.border_shape.SetStrokeThickness(thick);
        let _ = self.label.SetSize(size);
        // Whole device pixels keep the text crisp (whole DIPs do not, at
        // 125 or 150 %).
        let (x, y) = snap_px(x, y, self.scale);
        let _ = self.label.SetOffset(Vector3 { X: x, Y: y, Z: 0.0 });
        self.set_label(true);
    }
}

/// Room around the label for its E1 shadow (Elevation::reach: Flat v2's
/// far layer, y 3 + blur 9).
const LABEL_SHADOW_M: f32 = crate::theme::metrics::space::S4;

/// The label's corner radius: the theme's tooltip radius, at most a
/// capsule's.
pub(super) fn label_radius(theme: &crate::theme::Theme, h: f32) -> f32 {
    theme.radii().tip.min(crate::theme::capsule(h))
}

/// Pure: (x, y) DIP on whole device pixels at `scale`.
pub(super) fn snap_px(x: f32, y: f32, scale: f32) -> (f32, f32) {
    let s = if scale > 0.0 { scale } else { 1.0 };
    ((x * s).round() / s, (y * s).round() / s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_label_is_a_tooltip_on_whole_pixels() {
        let t = crate::theme::Theme::graphite();
        assert_eq!(label_radius(&t, 24.0), 8.0, "a tooltip's corners");
        assert_eq!(label_radius(&t, 8.0), 4.0, "never past a capsule");
        assert!(LABEL_SHADOW_M >= crate::theme::Elevation::E1.reach(), "its E1 shadow fits its room");
        for s in [1.0f32, 1.25, 1.5, 1.75, 2.0] {
            let (x, y) = snap_px(10.3, 7.77, s);
            assert!(((x * s) - (x * s).round()).abs() < 1e-3 && ((y * s) - (y * s).round()).abs() < 1e-3, "{s}");
        }
        // Its text: footnote 12, its box on the grid.
        assert_eq!(crate::gfx::LABEL_MIN_H % 4.0, 0.0);
        assert_eq!(crate::theme::ramp::FOOTNOTE.size, 12.0);
        // Its gap off the icon and its wait are the kit's.
        assert_eq!(LABEL_GAP, 8.0);
        assert_eq!(LABEL_DELAY_MS, 300);
    }

    /// A menu opening takes the label at once, whatever its fade is doing
    /// (7.4): no fade is started, the Opacity is assigned 0 (which stops a
    /// running one), and shown / warm agree with it.
    #[test]
    fn a_menu_cuts_the_label_whatever_its_fade_is_doing() {
        use crate::motion::fake::{Call, Fake};
        let t0 = Instant::now();
        let now = t0 + std::time::Duration::from_millis(40);
        // Coming in (0.4 of the way up): to 0 at once, closed now.
        let p = Fake::at(0.4f32);
        let (mut shown, mut closed) = (true, None);
        cut(&p, &mut shown, &mut closed, now);
        assert_eq!(p.calls(), vec![Call::Set(0.0)]);
        assert_eq!((p.shown.get(), shown, closed), (0.0, false, Some(now)));
        // Up in full: the same.
        let p = Fake::at(1.0f32);
        let (mut shown, mut closed) = (true, Some(t0));
        cut(&p, &mut shown, &mut closed, now);
        assert_eq!(p.calls(), vec![Call::Set(0.0)]);
        assert_eq!((p.shown.get(), shown, closed), (0.0, false, Some(now)));
        // Already going (0.7 left of a fade-out started at t0): cut short
        // to 0, and it went at t0 (the warm state counts from then).
        let p = Fake::at(0.7f32);
        let (mut shown, mut closed) = (false, Some(t0));
        cut(&p, &mut shown, &mut closed, now);
        assert_eq!(p.calls(), vec![Call::Set(0.0)]);
        assert_eq!((p.shown.get(), shown, closed), (0.0, false, Some(t0)));
        // Never shown: 0, not warm.
        let p = Fake::at(0.0f32);
        let (mut shown, mut closed) = (false, None);
        cut(&p, &mut shown, &mut closed, now);
        assert_eq!((p.shown.get(), shown, closed), (0.0, false, None));
        assert!(!warm(closed.map(|t| now - t)));
    }

    #[test]
    fn a_label_closed_a_moment_ago_skips_the_wait() {
        use std::time::Duration;
        assert!(!warm(None), "none shown yet: the wait");
        assert!(warm(Some(Duration::from_millis(0))));
        assert!(warm(Some(Duration::from_millis(499))));
        assert!(!warm(Some(Duration::from_millis(500))));
        assert!(!warm(Some(Duration::from_secs(60))));
    }

    #[test]
    fn the_rim_is_one_device_pixel_inside_the_edge() {
        use crate::syslook::SysLook;
        use crate::theme::{Finish, Material, Theme};
        for light in [false, true] {
            // A glass: 150 %, a stroke 1 / 1.5 DIP thick, half of it in
            // from the edge, concentric with the pill's corners.
            let mut raw = Theme::graphite();
            raw.finish = Finish::Glass;
            let glass = raw.resolve_fx(&SysLook { light, ..Default::default() }, true);
            let (inset, (w, h), r, t) = edge_stroke(60.0, 24.0, 8.0, glass.rim_width(Material::Tip, 1.5), false);
            assert!((t * 1.5 - 1.0).abs() < 1e-6 && (inset - t / 2.0).abs() < 1e-6);
            assert!((w - (60.0 - t)).abs() < 1e-6 && (h - (24.0 - t)).abs() < 1e-6 && (r - (8.0 - t / 2.0)).abs() < 1e-6);
            // Flat: its edge stroke is 1 DIP at every scale (clear since
            // 2026-11-12).
            let flat = Theme::graphite().resolve_fx(&SysLook { light, ..Default::default() }, true);
            for s in [1.0, 1.5, 2.0] {
                let (inset, _, r, t) = edge_stroke(60.0, 24.0, 8.0, flat.rim_width(Material::Tip, s), false);
                assert_eq!((inset, r, t), (0.5, 7.5, 1.0), "{s}");
            }
            // High contrast: a 1 DIP outline at every scale.
            assert_eq!(edge_stroke(60.0, 24.0, 8.0, glass.rim_width(Material::Tip, 1.5), true), (0.5, (59.0, 23.0), 7.5, 1.0));
        }
    }
}

/// Pictures for the design audit (tests only).
#[cfg(test)]
#[path = "label_harmony.rs"]
mod harmony;
#[cfg(test)]
pub(crate) use harmony::font_compare as font_compare_label;
