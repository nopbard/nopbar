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

/// The cell's width (DIPs).
const CELL_W: f32 = 20.0;

/// The same paint is used by the live surface and the offscreen audit.
/// The cell is CELL_W x `h` DIPs, drawn at `s` device px per DIP; `line`:
/// high contrast (Picture::Mark is then the icon's outline in `ink`).
unsafe fn paint_cell(dc: &windows::Win32::Graphics::Direct2D::ID2D1DeviceContext, art: Picture, h: f32, ink: crate::theme::Rgba, s: f32, line: bool) -> Result<()> {
    let color = D2D1_COLOR_F { r: ink.r as f32 / 255., g: ink.g as f32 / 255., b: ink.b as f32 / 255., a: 1. };
    match art {
        Picture::Mark => {
            if paint_own(dc, h, ink, s, line).is_err() {
                crate::glyphs::draw(dc, Glyph::Mark, CELL_W / 2., h / 2., h, color)?;
            }
        }
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

/// NopBar's own tray picture (own_mark: the app icon, or with `line` its
/// outline in `ink`) at the tray icons' size (TRAY_DIP, the cell's height
/// at most), 1:1 on the device pixels and centered on the cell's.
unsafe fn paint_own(dc: &windows::Win32::Graphics::Direct2D::ID2D1DeviceContext, h: f32, ink: crate::theme::Rgba, s: f32, line: bool) -> Result<()> {
    use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U};
    use windows::Win32::Graphics::Direct2D::{D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1, D2D1_INTERPOLATION_MODE_LINEAR};
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
    let fail = || windows::core::Error::from(windows::Win32::Foundation::E_FAIL);
    let n = (TRAY_DIP.min(h) * s).round().max(1.0) as u32;
    let pixels = own_mark(n, line).ok_or_else(fail)?;
    let pixels = if line { tint_premultiplied(&pixels, ink) } else { pixels };
    let props = D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: 96. * s,
        dpiY: 96. * s,
        bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
        colorContext: std::mem::ManuallyDrop::new(None),
    };
    let bmp = dc.CreateBitmap(D2D_SIZE_U { width: n, height: n }, Some(pixels.as_ptr() as *const _), n * 4, &props)?;
    let side = n as f32 / s;
    let left = ((CELL_W * s - n as f32) / 2.).round() / s;
    let top = ((h * s - n as f32) / 2.).round() / s;
    dc.DrawBitmap(&bmp, Some(&D2D_RECT_F { left, top, right: left + side, bottom: top + side }), 1., D2D1_INTERPOLATION_MODE_LINEAR, None, None);
    Ok(())
}

#[cfg(test)]
pub(crate) unsafe fn paint_home(dc: &windows::Win32::Graphics::Direct2D::ID2D1DeviceContext, h: f32, ink: crate::theme::Rgba, notice: bool) -> Result<()> {
    paint_cell(dc, Picture::Home { notice }, h, ink, 1., false)
}

impl TopBar {
    pub(super) fn companion_surface(&mut self) -> Option<(CompositionSurfaceBrush, f32, f32)> {
        let h = (self.height - 8.0).max(12.0);
        let w = CELL_W;
        let art = picture(self.companion_enabled, crate::companion::visible(), crate::companion::attention());
        let line = self.theme.high_contrast;
        let key = (Style::Glyph, format!("companion|{art:?}|{line}"));
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
                    paint_cell(dc, art, h, self.ground.ink, s, line)
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
            .and_then(slot_anchor)
    })
}

fn slot_anchor(b: &TopBar) -> Option<crate::companion::SlotAnchor> {
    if b.hidden_full || !unsafe { IsWindowVisible(b.hwnd).as_bool() } { return None; }
    let rect = b.slot_rect(b.slot_of(Cell::Companion)?);
    (rect.bottom > b.monitor.top && rect.top < b.monitor.bottom)
        .then_some(crate::companion::SlotAnchor { rect, dpi: b.scale })
}

pub fn companion_news_anchor(preferred: Option<crate::companion::SlotAnchor>) -> Option<(crate::companion::SlotAnchor, RECT)> {
    BARS.with(|bars| {
        let bars = bars.try_borrow().ok()?;
        let visible = || bars.iter().filter_map(|b| slot_anchor(b).map(|slot| (b, slot)));
        let (bar, slot) = if let Some(old) = preferred {
            visible().find(|(b, _)| {
                old.rect.left >= b.monitor.left && old.rect.left < b.monitor.right
                    && old.rect.top >= b.monitor.top && old.rect.top < b.monitor.bottom
            })
        } else {
            let mouse = crate::monitors::under_cursor().map(|m| m.handle);
            visible().find(|(b, _)| Some(b.hmon) == mouse)
                .or_else(|| visible().find(|(b, _)| b.primary))
        }?;
        Some((slot, crate::monitors::info(bar.hmon).map_or(bar.monitor, |m| m.work)))
    })
}

pub fn companion_news_bounce(anchor: crate::companion::SlotAnchor) {
    BARS.with(|bars| {
        let Ok(bars) = bars.try_borrow() else { return; };
        if let Some(bar) = bars.iter().find(|b| slot_anchor(b).is_some_and(|slot| slot.rect == anchor.rect)) {
            bounce_slot(bar);
        }
    });
}

pub fn companion_changed() {
    BARS.with(|bars| {
        let Ok(mut bars) = bars.try_borrow_mut() else {
            return;
        };
        for b in bars.iter_mut() {
            b.update();
        }
    });
}

fn bounce_slot(bar: &TopBar) {
    let Some(i) = bar.slot_of(Cell::Companion) else { return; };
    let slot = &bar.slots[i];
    let _ = (|| -> Result<()> {
        let a = bar.gfx.compositor.CreateScalarKeyFrameAnimation()?;
        for (p, dy) in [(0., 0.), (0.25, -2.5), (0.5, 0.), (0.7, -1.), (1., 0.)] {
            a.InsertKeyFrame(p, slot.at.1 + dy)?;
        }
        a.SetDuration(windows::Foundation::TimeSpan { Duration: 7_000_000 })?;
        slot.visual.StartAnimation(&HSTRING::from("Offset.Y"), &a)?;
        Ok(())
    })();
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
