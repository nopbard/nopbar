use super::*;
use crate::companion::render::{scale, translate, Painter};
use windows::core::HSTRING;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Picture {
    Mark,
    Home { notice: bool },
}

fn picture(enabled: bool, away: bool, notice: bool) -> Picture {
    if !enabled || away { Picture::Mark } else { Picture::Home { notice } }
}

pub(super) fn content_key(enabled: bool) -> String {
    format!("companion|{:?}", picture(enabled, crate::companion::visible(), crate::companion::attention()))
}

/// Filter before assigning indices, IDs, automatic visibility or drag ordering.
pub(super) fn other_icons(mut entries: Vec<crate::tray::Entry>, own: impl Fn(u32) -> bool) -> Vec<crate::tray::Entry> {
    entries.retain(|e| !own(e.owner));
    entries
}

pub(super) fn click(bar: HWND, i: usize, right: bool, keyboard: bool) {
    let generation = GENERATION.load(std::sync::atomic::Ordering::SeqCst);
    let at = with_bar_of(bar, |b| {
        if b.slots.get(i)?.cell != Cell::Companion { return None; }
        b.drop_tip();
        let r = b.slot_rect(i);
        b.set_anchor(Some(Cell::Companion));
        Some(tray_click_point(r, b.at, b.scale, b.cfg.at_bottom(), work_area(mid_of(&r))))
    }).flatten();
    let Some(at) = at else { return };
    if !right && crate::companion::take_bar_cancel() {
        with_bar_of(bar, |b| if b.anchor == Some(Cell::Companion) { b.set_anchor(None); });
        return;
    }
    let _visit = crate::companion::MenuVisit::new();
    crate::dock::own_tray_click(at, right, keyboard);
    if GENERATION.load(std::sync::atomic::Ordering::SeqCst) == generation {
        with_bar_of(bar, |b| {
            if b.anchor == Some(Cell::Companion) { b.set_anchor(None); }
        });
    }
}

/// The same paint is used by the live surface and the offscreen audit.
unsafe fn paint_cell(dc: &windows::Win32::Graphics::Direct2D::ID2D1DeviceContext, art: Picture, h: f32, ink: crate::theme::Rgba) -> Result<()> {
    let color = D2D1_COLOR_F { r: ink.r as f32 / 255., g: ink.g as f32 / 255., b: ink.b as f32 / 255., a: 1. };
    match art {
        Picture::Mark => crate::glyphs::draw(dc, Glyph::Mark, 10., h / 2., h, color)?,
        Picture::Home { notice } => {
            Painter::new().slot(dc, 10., h, h, 1.)?;
            if notice {
                let brush = dc.CreateSolidColorBrush(&color, None)?;
                dc.FillEllipse(&windows::Win32::Graphics::Direct2D::D2D1_ELLIPSE {
                    point: windows_numerics::Vector2 { X: 17.75, Y: 2.25 },
                    radiusX: 2.25, radiusY: 2.25,
                }, &brush);
            }
        }
    }
    Ok(())
}

impl TopBar {
    pub(super) fn companion_surface(&mut self) -> Option<(CompositionSurfaceBrush, f32, f32)> {
        let h = (self.height - 8.0).max(12.0);
        let w = 20.0;
        let art = picture(self.companion_enabled, crate::companion::visible(), crate::companion::attention());
        let key = (Style::Glyph, format!("companion|{art:?}"));
        if let Some(s) = self.surfaces.get(&key) {
            return Some(s.clone());
        }
        let s = self.scale;
        let pic = self
            .gfx
            .draw(
                (w * s).ceil() as u32,
                (h * s).ceil() as u32,
                |dc, ox, oy| unsafe {
                    dc.SetTransform(&(scale(s, s) * translate(ox, oy)));
                    paint_cell(dc, art, h, self.ground.ink)
                },
            )
            .ok()?;
        let brush = self
            .gfx
            .compositor
            .CreateSurfaceBrushWithSurface(&pic)
            .ok()?;
        let _ = brush.SetStretch(CompositionStretch::Fill);
        let result = (brush, w, h);
        self.surfaces.insert(key, result.clone());
        Some(result)
    }
}
/// The slot on the dock's monitor, else the primary bar (bar disabled:
/// None, the companion uses the monitor's upper right as its home).
pub fn companion_anchor() -> Option<crate::companion::SlotAnchor> {
    BARS.with(|bars| {
        let bars = bars.try_borrow().ok()?;
        let monitor = crate::companion::monitor();
        bars.iter()
            .find(|b| Some(b.monitor) == monitor)
            .or_else(|| bars.iter().find(|b| b.primary))
            .filter(|b| !b.hidden_full && unsafe { IsWindowVisible(b.hwnd).as_bool() })
            .and_then(|b| b.slot_of(Cell::Companion).map(|i| (b, b.slot_rect(i))))
            .filter(|(b, r)| r.bottom > b.monitor.top && r.top < b.monitor.bottom)
            .map(|(b, rect)| crate::companion::SlotAnchor { rect, dpi: b.scale })
    })
}
pub fn companion_changed(bounce: bool) {
    BARS.with(|bars| {
        let Ok(mut bars) = bars.try_borrow_mut() else {
            return;
        };
        for b in bars.iter_mut() {
            b.update();
            if bounce && !b.hidden_full && crate::effects::motion() {
                if let Some(i) = b.slot_of(Cell::Companion) {
                    let slot = &b.slots[i];
                    let _ = (|| -> Result<()> {
                        let a = b.gfx.compositor.CreateScalarKeyFrameAnimation()?;
                        for (p, dy) in [
                            (0.0, 0.0),
                            (0.25, -2.5),
                            (0.5, 0.0),
                            (0.7, -1.0),
                            (1.0, 0.0),
                        ] {
                            a.InsertKeyFrame(p, slot.at.1 + dy)?;
                        }
                        a.SetDuration(windows::Foundation::TimeSpan {
                            Duration: 7_000_000,
                        })?;
                        slot.visual.StartAnimation(&HSTRING::from("Offset.Y"), &a)?;
                        Ok(())
                    })();
                }
            }
        }
    });
}

pub(super) const WM_AGENDA:u32=WM_APP+92;
pub fn companion_agenda(at:i64){BARS.with(|bars|{let bars=bars.borrow();if let Some(b)=bars.iter().find(|b|b.primary){unsafe{let _=PostMessageW(Some(b.hwnd),WM_AGENDA,WPARAM(0),LPARAM(at as isize));}}});}
pub fn companion_tip(gfx:&Rc<Gfx>,scale:f32,text:&str,at:POINT,bounds:RECT){BARS.with(|bars|{if let Ok(bars)=bars.try_borrow(){if let Some(b)=bars.iter().find(|b|b.primary){show_tip_wrapped(gfx,&b.theme,scale,text,at,true,bounds,320.*scale);}}});}

pub fn companion_slot_at(point:POINT)->bool{BARS.with(|bars|{bars.try_borrow().is_ok_and(|bars|bars.iter().filter(|b|!b.hidden_full&&unsafe{IsWindowVisible(b.hwnd).as_bool()}).any(|b|b.slot_of(Cell::Companion).is_some_and(|i|{let r=b.slot_rect(i);point.x>=r.left&&point.x<r.right&&point.y>=r.top&&point.y<r.bottom})))})}

/// Warning text changed; this does not update or lay out bar slots.
pub fn companion_tooltip_changed(){
    BARS.with(|bars|{if let Ok(mut bars)=bars.try_borrow_mut(){for b in bars.iter_mut(){
        if b.tip==Some(super::desktops::TipTarget::Companion){b.refresh_tip();}
    }}});
}

/// Visible bars and floating islands, including the satellite pill.
pub fn companion_obstacles()->Option<Vec<RECT>> {BARS.with(|bars|{
 let bars=bars.try_borrow().ok()?;let mut out=Vec::new();
 for b in bars.iter().filter(|b|!b.hidden_full&&unsafe{IsWindowVisible(b.hwnd).as_bool()}) {
  out.push(b.at);if let Some(r)=b.island_screen_rect(){out.push(r);}
 }Some(out)
})}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_icon_is_removed_before_mirror_layout() {
        let mut own = crate::tray::Entry::default(); own.owner = 7;
        let mut other = crate::tray::Entry::default(); other.owner = 9;
        let entries = other_icons(vec![own.clone(), other, own], |owner| owner == 7);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].owner, 9);
        assert!(other_icons(Vec::new(), |_| true).is_empty());
    }

    #[test]
    fn one_cell_selects_home_mark_and_notice() {
        for notice in [false, true] {
            assert_eq!(picture(false, false, notice), Picture::Mark);
            assert_eq!(picture(false, true, notice), Picture::Mark);
            assert_eq!(picture(true, true, notice), Picture::Mark);
            assert_eq!(picture(true, false, notice), Picture::Home { notice });
        }
        assert_ne!(picture(true, false, true), picture(true, false, false));
    }

    #[test]
    fn mouse_and_uia_use_the_tray_path() {
        let source = include_str!("mod.rs");
        assert!(source.contains("companion::click(bar, i, false, x_px.is_none())"));
        assert!(source.contains("companion::click(hwnd, i, true, false)"));
        assert!(!source.contains("crate::companion::slot_click()"));
        let access = include_str!("access.rs");
        assert!(access.contains("button(\"NopBar\".into()).menu()"));
        assert!(access.contains("companion::click(hwnd, i, true, true)"));
    }
}

#[cfg(test)]
#[path = "one_snapshots.rs"]
mod snapshots;
