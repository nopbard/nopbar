//! A panel keeps its opening position. Only display changes or overflow move it.
use windows::Win32::{Foundation::RECT, Graphics::Direct2D::Common::D2D_RECT_F};

/// Only a single, non-editable bubble of at most two text lines follows the pet.
pub const FOLLOW_LINES: u32 = 2;

pub fn is_panel(editing: bool, heights: impl Iterator<Item = f32>) -> bool {
    let heights: Vec<_> = heights.collect();
    editing
        || heights.len() > 1
        || heights.iter().any(|h| {
            *h > FOLLOW_LINES as f32 * super::chat::bubbles::LINE + 2. * super::chat::bubbles::PAD_Y
        })
}

pub fn clamp(work: RECT, size: (i32, i32), at: (i32, i32)) -> RECT {
    let x = at.0.clamp(work.left, (work.right - size.0).max(work.left));
    let y = at.1.clamp(work.top, (work.bottom - size.1).max(work.top));
    RECT {
        left: x,
        top: y,
        right: x + size.0,
        bottom: y + size.1,
    }
}

#[derive(Default)]
pub struct Pin {
    saved: Option<(RECT, f32, D2D_RECT_F)>,
}
impl Pin {
    pub fn reset(&mut self) {
        self.saved = None;
    }
    /// Returns the translation applied to both painted and interactive geometry.
    pub fn offset(&mut self, work: RECT, dpi: f32, bounds: D2D_RECT_F) -> (f32, f32) {
        let previous = self.saved.filter(|(w, d, _)| *w == work && *d == dpi);
        let at = previous.map_or(bounds, |(_, _, r)| r);
        let w = bounds.right - bounds.left;
        let h = bounds.bottom - bounds.top;
        let x = at.left.clamp(
            work.left as f32 / dpi,
            (work.right as f32 / dpi - w).max(work.left as f32 / dpi),
        );
        let y = at.top.clamp(
            work.top as f32 / dpi,
            (work.bottom as f32 / dpi - h).max(work.top as f32 / dpi),
        );
        self.saved = Some((
            work,
            dpi,
            D2D_RECT_F {
                left: x,
                top: y,
                right: x + w,
                bottom: y + h,
            },
        ));
        (x - bounds.left, y - bounds.top)
    }
}

#[derive(Default)]
pub struct ChatPlacement {
    pin: Pin,
    pinned: bool,
}
impl ChatPlacement {
    pub fn reset(&mut self) {
        self.pin.reset();
        self.pinned = false;
    }
    pub fn apply(
        &mut self,
        items: &mut [super::chat::paint::Item],
        work: RECT,
        dpi: f32,
        editing: bool,
        buttons: bool,
    ) {
        self.pinned |= buttons
            || is_panel(
                editing,
                items.iter().filter(|i| i.depth < 3).map(|i| i.text.height),
            );
        if !self.pinned || items.is_empty() {
            return;
        }
        let bounds = items.iter().fold(items[0].bounds, |r, i| D2D_RECT_F {
            left: r.left.min(i.bounds.left),
            top: r.top.min(i.bounds.top),
            right: r.right.max(i.bounds.right),
            bottom: r.bottom.max(i.bounds.bottom),
        });
        let padded = D2D_RECT_F {
            left: bounds.left - 4.,
            top: bounds.top - 4.,
            right: bounds.right + 4.,
            bottom: bounds.bottom + if buttons { 34. } else { 4. },
        };
        let (dx, dy) = self.pin.offset(work, dpi, padded);
        for item in items {
            item.bounds.left += dx;
            item.bounds.right += dx;
            item.bounds.top += dy;
            item.bounds.bottom += dy;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::companion::chat::{
        bubbles::{LINE, PAD_Y},
        paint::rect,
    };
    #[test]
    fn only_one_or_two_line_noneditable_bubbles_follow() {
        let two = 2. * LINE + 2. * PAD_Y;
        assert!(!is_panel(false, [two].into_iter()));
        assert!(is_panel(false, [two + LINE].into_iter()));
        assert!(is_panel(true, [two].into_iter()));
        assert!(is_panel(false, [two, two].into_iter()));
    }
    #[test]
    fn pin_survives_motion_and_growth_but_resets_for_display_or_reopen() {
        let work = RECT {
            left: 0,
            top: 0,
            right: 1000,
            bottom: 800,
        };
        let mut p = Pin::default();
        assert_eq!(p.offset(work, 1., rect(400., 500., 200., 100.)), (0., 0.));
        assert_eq!(
            p.offset(work, 1., rect(100., 200., 200., 100.)),
            (300., 300.)
        );
        assert_eq!(
            p.offset(work, 1., rect(100., 200., 200., 400.)),
            (300., 200.)
        );
        assert_eq!(p.offset(work, 2., rect(100., 100., 200., 100.)), (0., 0.));
        p.reset();
        assert_eq!(p.offset(work, 1., rect(10., 20., 200., 100.)), (0., 0.));
    }
}
