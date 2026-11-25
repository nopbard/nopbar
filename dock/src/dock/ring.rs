//! What is drawn over an icon, around it: the keyboard's focus ring
//! (harmony 6.4: 2 DIP wide, 2 DIP out of the icon, the icon's own
//! squircle, the ink at .80 or the accent; Highlight in high contrast),
//! shown at once while the dock has the keyboard (access.rs), and high
//! contrast's pressed outline (12.3: Highlight, 2 DIP, inside the icon,
//! where the other looks dim the icon).
//!
//! Each is one sprite over the icons. Its picture is drawn for the icon's
//! largest size (the focused or pressed icon stands magnified) and
//! stretched onto the icon as it is drawn now.

use super::*;

/// The pressed outline's width in high contrast (12.3: a state's outline).
const PRESS_W: f32 = 2.0;

pub(super) struct Rings {
    layer: ContainerVisual,
    focus: SpriteVisual,
    press: SpriteVisual,
    /// Pictures by (side px, stroke px x 100, color).
    pics: HashMap<(u32, u32, u32), CompositionSurfaceBrush>,
}

impl Rings {
    pub fn new(c: &Compositor) -> Result<Self> {
        let layer = c.CreateContainerVisual()?;
        let (focus, press) = (c.CreateSpriteVisual()?, c.CreateSpriteVisual()?);
        for v in [&focus, &press] {
            v.SetOpacity(0.0)?;
            layer.Children()?.InsertAtTop(v)?;
        }
        Ok(Self { layer, focus, press, pics: HashMap::new() })
    }

    pub fn layer(&self) -> &ContainerVisual {
        &self.layer
    }
}

/// Pure: a ring's box around an icon drawn at (`x`, `y`), `side` DIP
/// wide, reaching `out` DIP past it: (x, y, side).
pub(super) fn ring_box(x: f32, y: f32, side: f32, out: f32) -> (f32, f32, f32) {
    (x - out, y - out, side + out * 2.0)
}

impl Dock {
    /// The icon the keyboard's focus is on, its ring shown (keyboard mode
    /// only: Dock::kb_ring).
    fn focused_item(&self) -> Option<usize> {
        let key = self.kb.as_ref().filter(|_| self.kb_ring)?;
        self.item_keys().iter().position(|k| k == key)
    }

    /// Puts the rings where they belong (after every layout).
    pub(super) fn place_rings(&mut self) {
        let (out, w) = crate::gfx::parts::icon_ring();
        let focus = if self.hidden { None } else { self.focused_item() };
        self.place_ring(false, focus, out, w, crate::gfx::parts::focus_color(&self.theme));
        // High contrast: the pressed icon keeps its brightness and takes
        // an outline (not while dragged: the drag shows itself).
        let hc = self.theme.focus_outline();
        let pressed = if self.drag.is_none() { self.pressed.map(|(i, _)| i) } else { None };
        match hc {
            Some(color) => self.place_ring(true, pressed, 0.0, PRESS_W, color),
            None => {
                let _ = self.rings.press.SetOpacity(0.0);
            }
        }
    }

    fn place_ring(&mut self, press: bool, at: Option<usize>, out: f32, stroke: f32, color: crate::theme::Rgba) {
        let v = if press { self.rings.press.clone() } else { self.rings.focus.clone() };
        let drawn = at.and_then(|i| self.items.get(i)).filter(|it| !it.is_line()).map(|it| (it.plate.Offset(), it.plate.Size()));
        let Some((Ok(o), Ok(sz))) = drawn else {
            let _ = v.SetOpacity(0.0);
            return;
        };
        // Drawn for the icon at its largest, its stroke `stroke` DIP there.
        let peak = self.look.icon * self.look.max_scale + out * 2.0;
        let side_px = (peak * self.scale).round().max(1.0) as u32;
        let stroke_px = stroke * self.scale;
        let key = (side_px, (stroke_px * 100.0).round() as u32, u32::from_be_bytes([color.r, color.g, color.b, color.a]));
        if !self.rings.pics.contains_key(&key) {
            let Ok(surface) = self.gfx.squircle_ring(side_px, stroke_px, color) else { return };
            let Ok(brush) = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface) else { return };
            let _ = brush.SetStretch(CompositionStretch::Fill);
            if self.rings.pics.len() > 8 {
                self.rings.pics.clear();
            }
            self.rings.pics.insert(key, brush);
        }
        let Some(brush) = self.rings.pics.get(&key) else { return };
        let (x, y, side) = ring_box(o.X, o.Y, sz.X.min(sz.Y), out);
        let _ = v.SetBrush(brush);
        let _ = v.SetOffset(Vector3 { X: x, Y: y, Z: 0.0 });
        let _ = v.SetSize(Vector2 { X: side, Y: side });
        // At once: the keyboard's changes leave no trail (6.3).
        let _ = v.SetOpacity(1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_focus_ring_stands_two_dip_off_the_icon() {
        // 6.4: 2 wide, 2 out: it reaches 4 past the icon on every side.
        let (out, w) = crate::gfx::parts::icon_ring();
        assert_eq!((out, w), (4.0, 2.0));
        assert_eq!(ring_box(100.0, 20.0, 52.0, out), (96.0, 16.0, 60.0));
        // High contrast's pressed outline is the icon's own box.
        assert_eq!(ring_box(100.0, 20.0, 52.0, 0.0), (100.0, 20.0, 52.0));
    }
}
