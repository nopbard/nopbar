//! Drawing the panel's surface: the grid (layout.rs) or the list it went
//! into, as the design kit's harmony A has it.

use super::*;

/// What a button is (design system 6.1): raised (a tile, the output
/// button: a card's fill at rest), a chip on a card, or part of the
/// surface (a glyph button: no fill at rest).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Face {
    Chip,
    Glyph,
}

impl Panel {
    /// A text line `size` DIP tall. gfx grows text by the user's text
    /// size; here the whole panel does (panel_size), so that is taken out
    /// again: text keeps its share of its line box at every text size.
    pub(super) fn layout_text(&self, text: &str, size: f32, bold: bool) -> Option<(IDWriteTextLayout, f32, f32)> {
        let weight = if bold { DWRITE_FONT_WEIGHT_SEMI_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL };
        self.gfx.text_layout(text, &self.theme.font.family, size / crate::fonts::scale(), weight).ok()
    }

    /// Text cut with "…" to fit `maxw` DIP.
    pub(super) fn fit_text(&self, text: &str, size: f32, bold: bool, maxw: f32) -> Option<(IDWriteTextLayout, f32, f32)> {
        let l = self.layout_text(text, size, bold)?;
        if l.1 <= maxw {
            return Some(l);
        }
        let chars: Vec<char> = text.chars().collect();
        let cut = |n: usize| format!("{}…", chars[..n].iter().collect::<String>().trim_end());
        let (mut lo, mut hi) = (0usize, chars.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if self.layout_text(&cut(mid), size, bold).is_some_and(|l| l.1 <= maxw) {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        self.layout_text(&cut(lo), size, bold)
    }

    /// Numbers that keep their width (values, percents).
    pub(super) fn tabular(&self, text: &str, size: f32, bold: bool) -> Option<(IDWriteTextLayout, f32, f32)> {
        let weight = if bold { DWRITE_FONT_WEIGHT_SEMI_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL };
        self.gfx.text_layout_tabular(text, &self.theme.font.family, size / crate::fonts::scale(), weight).ok()
    }

    pub(super) unsafe fn fill_rr(&self, dc: &ID2D1DeviceContext, c: D2D1_COLOR_F, r: D2D_RECT_F, rad: f32) -> Result<()> {
        if c.a > 0.0 {
            let b = dc.CreateSolidColorBrush(&c, None)?;
            dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: rad, radiusY: rad }, &b);
        }
        Ok(())
    }

    /// An outline `width` wide inside `r` (its corners concentric).
    pub(super) unsafe fn stroke_rr(&self, dc: &ID2D1DeviceContext, c: D2D1_COLOR_F, r: D2D_RECT_F, rad: f32, width: f32) -> Result<()> {
        let b = dc.CreateSolidColorBrush(&c, None)?;
        let h = width / 2.0;
        let r = rect(r.left + h, r.top + h, r.right - h, r.bottom - h);
        dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: (rad - h).max(0.0), radiusY: (rad - h).max(0.0) }, &b, width, None);
        Ok(())
    }

    /// A soft shadow under an opaque rounded shape at elevation `e`
    /// (Theme::elevation's layers; none in high contrast or with the
    /// "Shadows" effect off).
    pub(super) unsafe fn shadow(&self, dc: &ID2D1DeviceContext, r: D2D_RECT_F, rad: f32, e: Elevation) -> Result<()> {
        if self.hc().is_some() || !crate::effects::items().shadows {
            return Ok(());
        }
        crate::gfx::soft_shadows(dc, r, rad, &self.theme.elevation(e))
    }

    /// The theme's radius for `k`: tiles and cards (12), the rows inside a
    /// card (concentric with its padding: 8), content (the art: 8).
    pub(super) fn radius(&self, k: Radius) -> f32 {
        let r = self.theme.radii();
        match k {
            Radius::Tile => PANEL_CORNER,
            Radius::Row => crate::theme::concentric(r.tile, LIST_PAD),
            Radius::Control => r.control,
        }
    }

    /// A text line drawn where the layout says (right to left: unmirrored,
    /// at its mirrored place: the layout is exactly its text's width).
    pub(super) unsafe fn draw_text(&self, dc: &ID2D1DeviceContext, l: &IDWriteTextLayout, x: f32, y: f32, c: D2D1_COLOR_F) -> Result<()> {
        let b = dc.CreateSolidColorBrush(&c, None)?;
        if self.rtl {
            let w = l.GetMaxWidth();
            self.unmirrored(dc, || crate::gfx::draw_text(dc, Vector2 { X: W - x - w, Y: y }, l, &b, D2D1_DRAW_TEXT_OPTIONS_NONE));
        } else {
            crate::gfx::draw_text(dc, Vector2 { X: x, Y: y }, l, &b, D2D1_DRAW_TEXT_OPTIONS_NONE);
        }
        Ok(())
    }

    /// A text line drawn as is (the parts are never mirrored).
    pub(super) unsafe fn draw_plain(&self, dc: &ID2D1DeviceContext, l: &IDWriteTextLayout, x: f32, y: f32, c: D2D1_COLOR_F) -> Result<()> {
        let b = dc.CreateSolidColorBrush(&c, None)?;
        crate::gfx::draw_text(dc, Vector2 { X: x, Y: y }, l, &b, D2D1_DRAW_TEXT_OPTIONS_NONE);
        Ok(())
    }

    /// A line of text in a line box (`top`, `box_h` tall), from `x`, cut
    /// to `maxw`. Its width.
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn line(&self, dc: &ID2D1DeviceContext, text: &str, size: f32, bold: bool, x: f32, top: f32, box_h: f32, maxw: f32, c: D2D1_COLOR_F) -> Result<f32> {
        let Some((l, w, h)) = self.fit_text(text, size, bold, maxw) else { return Ok(0.0) };
        self.draw_text(dc, &l, x, top + (box_h - h) / 2.0, c)?;
        Ok(w)
    }

    /// A line of text ending at `end` (its box `top`, `box_h` tall), cut to
    /// `maxw`. Its width.
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn line_end(&self, dc: &ID2D1DeviceContext, text: &str, size: f32, end: f32, top: f32, box_h: f32, maxw: f32, c: D2D1_COLOR_F) -> Result<f32> {
        let Some((l, w, h)) = self.fit_text(text, size, false, maxw) else { return Ok(0.0) };
        self.draw_text(dc, &l, end - w, top + (box_h - h) / 2.0, c)?;
        Ok(w)
    }

    /// Runs `f` with the panel's transform without its mirroring (right to
    /// left), in panel DIPs.
    pub(super) unsafe fn unmirrored<R>(&self, dc: &ID2D1DeviceContext, f: impl FnOnce() -> R) -> R {
        let mut m = Matrix3x2::default();
        dc.GetTransform(&mut m);
        dc.SetTransform(&unmirror(m));
        let r = f();
        dc.SetTransform(&m);
        r
    }

    /// A glyph centered on (cx, cy).
    pub(super) unsafe fn glyph(&self, dc: &ID2D1DeviceContext, g: Glyph, size: f32, cx: f32, cy: f32, c: D2D1_COLOR_F) -> Result<()> {
        crate::gfx::draw_glyph(dc, g, cx, cy, size, c)
    }

    /// A bitmap of `w` x `h` premultiplied pixels on `dc`'s device.
    pub(super) unsafe fn bitmap(dc: &ID2D1DeviceContext, w: u32, h: u32, pixels: &[u8]) -> Result<ID2D1Bitmap1> {
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
            colorContext: std::mem::ManuallyDrop::new(None),
        };
        dc.CreateBitmap(D2D_SIZE_U { width: w, height: h }, Some(pixels.as_ptr() as _), w * 4, &props)
    }

    /// The keyboard's focus ring (6.4) around `r` (corners `rad`), when the
    /// keyboard is on `h`.
    pub(super) unsafe fn ring(&self, dc: &ID2D1DeviceContext, h: Hit, r: D2D_RECT_F, rad: f32) -> Result<()> {
        if self.keyboard && self.focus == Some(h) {
            crate::gfx::parts::focus_ring(dc, r, rad, &self.theme, 1.0)?;
        }
        Ok(())
    }

    /// A card that only holds things (the connections', a strip, a list):
    /// the card's rest fill; in high contrast an outline of WindowText.
    pub(super) unsafe fn card(&self, dc: &ID2D1DeviceContext, r: D2D_RECT_F, tn: &Tones) -> Result<()> {
        let rad = self.radius(Radius::Tile);
        match self.hc() {
            Some(hc) => self.stroke_rr(dc, rgba(hc.text), r, rad, 1.0),
            None => {
                // (Flat v2's card light round it: gfx::parts::card.)
                crate::gfx::parts::card(dc, r, rad, &self.theme, crate::theme::State::Rest, tn.tile, 1.0, 1.0)
            }
        }
    }

    /// Control `h`'s state now: hovered by the pointer, pressed (the press
    /// that began on it, while the pointer is still on it), `on`.
    pub(super) fn look(&self, h: Hit, on: bool, enabled: bool) -> Look {
        let hover = enabled && self.hover == Some(h);
        Look { on, hover, pressed: hover && self.pressed == Some(h) }
    }

    /// A button drawn as a part of its own (it presses and cross-fades):
    /// its face in `face`'s colors for `look` (high contrast: 12.3), its
    /// glyph, the "opens elsewhere" mark, and the focus ring.
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn button(&self, dc: &ID2D1DeviceContext, tn: &Tones, hit: Hit, r: D2D_RECT_F, _rad: f32, glyph: Glyph, gsize: f32, face: Face, look: Look, enabled: bool, mark: bool) -> Result<()> {
        let rad = self.theme.radii().control;
        let (fill, ring, ink) = match self.hc() {
            Some(hc) => hc_tone(&hc, look, face != Face::Glyph),
            None => {
                let (f, i) = match face {
                    Face::Chip => chip_tone(tn, look),
                    Face::Glyph => glyph_tone(tn, look),
                };
                (f, None, i)
            }
        };
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        self.plan.borrow_mut().push(Spec::Disc(DiscSpec {
            hit,
            r,
            rad,
            glyph,
            gsize,
            fill,
            ring,
            ink,
            look,
            press: crate::motion::press_scale(w, h),
            dim: !enabled,
            mark,
            lit: (0, 0),
        }));
        self.ring(dc, hit, r, rad)
    }

    /// A row's fill (part of the surface, 6.2): the row's hover and
    /// pressed fills; high contrast an outline of Highlight 2 inside on
    /// hover and Highlight pressed (its ink HighlightText then). The ink.
    pub(super) unsafe fn row_face(&self, dc: &ID2D1DeviceContext, tn: &Tones, r: D2D_RECT_F, hover: bool, pressed: bool) -> Result<D2D1_COLOR_F> {
        let rad = self.radius(Radius::Row);
        Ok(match self.hc() {
            Some(hc) if pressed => {
                self.fill_rr(dc, rgba(hc.highlight), r, rad)?;
                rgba(hc.highlight_text)
            }
            Some(hc) => {
                if hover {
                    self.stroke_rr(dc, rgba(hc.highlight), r, rad, 2.0)?;
                }
                rgba(hc.text)
            }
            None => {
                let f = if pressed { tn.row_press } else if hover { tn.row_hover } else { Rgba { a: 0, ..tn.row_hover } };
                self.fill_rr(dc, rgba(f), r, rad)?;
                rgba(tn.ink)
            }
        })
    }

    /// An app's icon in `r` (fitted, unmirrored), uploaded once per icon.
    pub(super) unsafe fn app_icon(&self, dc: &ID2D1DeviceContext, icon: &Arc<AppIcon>, r: D2D_RECT_F) -> Result<()> {
        if crate::gfx::bgra_len(icon.w, icon.h) != Some(icon.pixels.len()) {
            return Ok(());
        }
        let device = dc.GetDevice()?;
        let bmp = {
            let mut cache = self.icon_bmps.borrow_mut();
            match cache.iter().find(|c| Arc::ptr_eq(&c.0, icon) && c.1.as_raw() == device.as_raw()) {
                Some(c) => c.2.clone(),
                None => {
                    let b = Self::bitmap(dc, icon.w, icon.h, &icon.pixels)?;
                    cache.retain(|c| !Arc::ptr_eq(&c.0, icon));
                    cache.push((icon.clone(), device, b.clone()));
                    b
                }
            }
        };
        let (fw, fh) = crate::icons::fit_box(icon.w, icon.h, 64);
        let (bw, bh) = (r.right - r.left, r.bottom - r.top);
        let (dw, dh) = (bw * fw / 64.0, bh * fh / 64.0);
        let (l, rr) = crate::i18n::mirror::edges(self.rtl, r.left, r.right, 0.0, W);
        let cx = (l + rr) / 2.0;
        let cy = (r.top + r.bottom) / 2.0;
        let dest = rect(cx - dw / 2.0, cy - dh / 2.0, cx + dw / 2.0, cy + dh / 2.0);
        self.unmirrored(dc, || dc.DrawBitmap(&bmp, Some(&dest), 1.0, windows::Win32::Graphics::Direct2D::D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None));
        Ok(())
    }

    /// Draws the view shown now in DIPs and records what can be clicked;
    /// the animated parts are asked for (self.plan) and drawn apart.
    pub(super) unsafe fn paint(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, sliders: &mut Vec<(Hit, D2D_RECT_F)>, r: &Remote, lay: &Layout) -> Result<()> {
        self.texts.borrow_mut().clear();
        self.inert.borrow_mut().clear();
        self.values.borrow_mut().clear();
        self.tips.borrow_mut().clear();
        self.plan.borrow_mut().clear();
        let tn = self.tones();
        dc.PushAxisAlignedClip(&rect(0.0, 0.0, W, self.height), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        let result = match self.open {
            Some(s) => self.paint_list(dc, hits, sliders, r, &tn, s),
            None => self.paint_grid(dc, hits, sliders, r, &tn, lay),
        };
        dc.PopAxisAlignedClip();
        result
    }

    unsafe fn paint_grid(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, sliders: &mut Vec<(Hit, D2D_RECT_F)>, r: &Remote, tn: &Tones, lay: &Layout) -> Result<()> {
        let rows = removable_now();
        for &(item, rc) in &lay.items {
            let rc = shifted_y(rc, -self.scroll);
            match item {
                Item::Connections => self.paint_connections(dc, hits, r, tn, rc)?,
                Item::Tile(h) => self.paint_tile(dc, hits, r, tn, h, rc)?,
                Item::Brightness | Item::Volume => self.paint_slider(dc, hits, sliders, r, tn, if item == Item::Volume { Hit::Volume } else { Hit::Brightness }, rc)?,
                Item::Media => self.paint_media(dc, hits, r, tn, rc)?,
                Item::Eject(i) => {
                    if let Some(row) = rows.get(i) {
                        self.paint_eject(dc, hits, tn, row, rc)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// A line of text screen readers get as drawn (see Panel::texts).
    pub(super) fn say(&self, hits: &[(D2D_RECT_F, Hit)], key: &'static str, text: &str, r: D2D_RECT_F) {
        if !text.is_empty() {
            self.texts.borrow_mut().push((hits.len(), r, key, text.to_string()));
        }
    }

    /// The status line drawn with control `h` (see Panel::values).
    pub(super) fn say_for(&self, h: Hit, text: &str) {
        if !text.is_empty() {
            self.values.borrow_mut().push((h, text.to_string()));
        }
    }

    /// Control `h`'s tooltip.
    pub(super) fn tip(&self, h: Hit, text: String) {
        if !text.is_empty() {
            self.tips.borrow_mut().push((h, text));
        }
    }

    pub(super) fn tile_view(&self, hit: Hit, r: &Remote) -> TileView {
        let now = Instant::now();
        let mut tile = TileView {
            glyph: G_INFO, label: "", note: None, on: false, enabled: true,
            disclosure: false, external: false,
        };
        tile.label = match hit {
            Hit::WifiToggle | Hit::BtToggle => {
                let wifi = hit == Hit::WifiToggle;
                let i = usize::from(!wifi);
                let radio = if wifi { r.wifi } else { r.bt };
                tile.glyph = if wifi { G_WIFI } else { G_BLUETOOTH };
                tile.on = radio.on();
                tile.note = switch_note(r.radio_busy[i], r.radio_failed[i], now).or_else(|| radio_reason(radio));
                tile.enabled = radio != RadioView::Loading;
                t(if wifi { "control.wifi" } else { "control.bluetooth" })
            }
            Hit::Quick(q) => {
                let view = quick_view(q, r, now);
                tile.glyph = quick_glyph(q);
                tile.on = view.on;
                tile.enabled = view.ready;
                tile.note = view.status.filter(|k| !matches!(*k, "control.on" | "control.off"));
                tile.external = q == Quick::EnergySaver;
                t(quick_label(q))
            }
            Hit::Dark => {
                tile.glyph = G_MOON;
                tile.on = self.dark;
                tile.note = switch_note(self.dark_seq != self.dark_done, self.dark_failed, now);
                t("control.dark_mode")
            }
            Hit::AccessList => {
                tile.glyph = Glyph::Accessibility;
                tile.on = r.access.iter().any(|v| v.on == Some(true));
                tile.disclosure = true;
                t("control.accessibility")
            }
            Hit::Project => {
                tile.glyph = Glyph::Project;
                tile.disclosure = true;
                t("control.project")
            }
            Hit::Studio => {
                tile.glyph = Glyph::Camera;
                tile.external = true;
                t("control.studio_effects")
            }
            _ => {
                tile.glyph = G_CAST;
                t("control.screen_cast")
            }
        };
        tile
    }

    unsafe fn paint_connections(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, r: &Remote, tn: &Tones, card: D2D_RECT_F) -> Result<()> {
        self.card(dc, card, tn)?;
        let rows = connection_rows(r);
        for (hit, row) in rows.iter().copied().zip(connection_rects(card, rows.len())) {
            let view = self.tile_view(hit, r);
            let navigation = match hit {
                Hit::WifiToggle => Some(Hit::WifiName),
                Hit::BtToggle => Some(Hit::BtName),
                _ => None,
            };
            let chip = connection_chip(row);
            let target = if navigation.is_some() { chip } else { row };
            add_control(hits, &mut self.inert.borrow_mut(), target, hit, view.enabled);
            let tail = rect(chip.right, row.top, row.right, row.bottom);
            if let Some(h) = navigation {
                hits.push((tail, h));
            }
            let look = self.look(navigation.unwrap_or(hit), view.on, navigation.is_some() || view.enabled);
            let ink = self.row_face(dc, tn, row, look.hover, look.pressed)?;
            let mut chip_look = self.look(hit, view.on, view.enabled);
            if navigation.is_some() && self.hc().is_some() && look.pressed {
                chip_look.on = true;
            }
            self.button(dc, tn, hit, chip, TILE_CORNER, view.glyph, CHIP_GLYPH, Face::Chip, chip_look, view.enabled, false)?;

            let x = chip.right + ROW_IN;
            let mut end = row.right - ROW_IN / 2.0;
            let cy = (row.top + row.bottom) / 2.0;
            let dim = if look.pressed && self.hc().is_some() { ink } else { rgba(tn.dim) };
            if navigation.is_some() {
                let g = if self.rtl { Glyph::ChevronLeft } else { Glyph::ChevronRight };
                self.glyph(dc, g, MARK_GLYPH, end - MARK_GLYPH / 2.0, cy, dim)?;
                end -= MARK_GLYPH + S2;
            }
            let (summary, additional) = navigation.map_or(("", 0), |h| radio_summary(r, h == Hit::WifiName));
            let note = view.note.map(t).unwrap_or(summary);
            let count = (view.note.is_none() && additional != 0).then(|| format!("+{additional}"));
            let width = (end - x).max(0.0);
            let label_top = cy - TITLE_H / 2.0;
            let inline_wifi = navigation == Some(Hit::WifiName) && view.note.is_none()
                && self.layout_text(view.label, T_NAME, true)
                    .zip(self.layout_text(note, T_SMALL, false))
                    .is_some_and(|(label, ssid)| label.1 + ssid.1 + S2 <= width);
            if note.is_empty() || inline_wifi {
                let name_width = self.line(dc, view.label, T_NAME, true, x, label_top, TITLE_H, width, ink)?;
                if inline_wifi {
                    self.line_end(dc, note, T_SMALL, end, label_top, TITLE_H, (width - name_width - S2).max(0.0), dim)?;
                }
            } else {
                let (title_top, note_top) = two_lines(cy);
                self.line(dc, view.label, T_NAME, true, x, title_top, TITLE_H, width, ink)?;
                let count_width = if let Some(count) = &count {
                    self.line_end(dc, count, T_SMALL, end, note_top, ARTIST_H, width, dim)? + S2
                } else {
                    0.0
                };
                self.line(dc, note, T_SMALL, false, x, note_top, ARTIST_H, (width - count_width).max(0.0), dim)?;
            }
            let value = count.map_or_else(|| note.to_owned(), |count| format!("{note} {count}"));
            let tip = if value.is_empty() { view.label.to_owned() } else { format!("{} · {value}", view.label) };
            self.tip(hit, tip.clone());
            self.say_for(hit, &value);
            if let Some(h) = navigation {
                self.tip(h, tip);
                self.say_for(h, &value);
                self.ring(dc, h, tail, self.radius(Radius::Row))?;
            }
        }
        Ok(())
    }

    unsafe fn paint_tile(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, r: &Remote, tn: &Tones, hit: Hit, rc: D2D_RECT_F) -> Result<()> {
        let view = self.tile_view(hit, r);
        add_control(hits, &mut self.inert.borrow_mut(), rc, hit, view.enabled);
        let look = self.look(hit, view.on, view.enabled);
        let (fill, outline, fg) = match self.hc() {
            Some(hc) => hc_tone(&hc, look, true),
            None => {
                let (fill, ink) = raised_tone(tn, look);
                (fill, None, ink)
            }
        };
        if let Some(width) = outline {
            self.stroke_rr(dc, rgba(fill), rc, TILE_CORNER, width)?;
        } else {
            self.fill_rr(dc, rgba(fill), rc, TILE_CORNER)?;
        }
        let mut ink = rgba(fg);
        if !view.enabled {
            ink.a *= DIM;
        }
        let (cx, cy) = ((rc.left + rc.right) / 2.0, (rc.top + rc.bottom) / 2.0);
        let arrow = if self.rtl { Glyph::ChevronLeft } else { Glyph::ChevronRight };
        let value = if hit == Hit::Project { project_status(r.displays) } else { view.note }.map(t).unwrap_or("");
        if hit == Hit::Project {
            let gx = rc.left + STRIP_IN + TILE_GLYPH / 2.0;
            let x = rc.left + STRIP_IN + TILE_GLYPH + MEDIA_IN;
            let ax = rc.right - MEDIA_IN - MARK_GLYPH / 2.0;
            let width = (ax - MARK_GLYPH / 2.0 - ROW_IN - x).max(0.0);
            let dim = if self.hc().is_some() { ink } else { rgba(tn.dim) };
            self.glyph(dc, view.glyph, TILE_GLYPH, gx, cy, ink)?;
            self.glyph(dc, arrow, MARK_GLYPH, ax, cy, dim)?;
            if value.is_empty() {
                self.line(dc, view.label, T_NAME, true, x, cy - TITLE_H / 2.0, TITLE_H, width, ink)?;
            } else {
                let (title_top, note_top) = two_lines(cy);
                self.line(dc, view.label, T_NAME, true, x, title_top, TITLE_H, width, ink)?;
                self.line(dc, value, T_SMALL, false, x, note_top, ARTIST_H, width, dim)?;
            }
        } else {
            self.glyph(dc, view.glyph, TILE_GLYPH, cx, cy, ink)?;
            if view.external || view.disclosure {
                let mark = if view.external { G_EXTERNAL } else { arrow };
                self.glyph(dc, mark, EXT_MARK, rc.right - EXT_IN - EXT_MARK / 2.0, rc.top + EXT_IN + EXT_MARK / 2.0, ink)?;
            }
        }
        let tip = if value.is_empty() { view.label.to_owned() } else { format!("{} · {value}", view.label) };
        self.tip(hit, tip);
        self.say_for(hit, value);
        self.ring(dc, hit, rc, TILE_CORNER)
    }

    pub(super) fn pill_spec(&self, hit: Hit, sr: D2D_RECT_F, r: &Remote, tn: &Tones) -> PillSpec {
        let percent = |p: u8| tf("control.percent", &[("p", &p.to_string())]);
        let (v, value, glyph, enabled, muted) = match hit {
            Hit::Brightness => {
                let b = r.brightness.flatten().unwrap_or(0);
                (b as f32 / 100.0, percent(b), G_SUN, true, false)
            }
            _ => match self.volume {
                Some((p, m)) => (p as f32 / 100.0, if m { t("control.muted").to_string() } else { percent(p) }, crate::topbar::status::volume_glyph(p, m), true, m),
                None => (0.0, "—".to_string(), crate::topbar::status::volume_glyph(0, true), false, false),
            },
        };
        let held = self.sliding == Some(hit);
        let show_value = held || self.hover == Some(hit) || (hit == Hit::Volume && self.hover == Some(Hit::Mute)) || (self.keyboard && self.focus == Some(hit));
        let mute = (hit == Hit::Volume).then(|| {
            let l = self.look(Hit::Mute, false, enabled);
            (l.hover, l.pressed)
        });
        PillSpec {
            hit,
            r: sr,
            held,
            look: PillLook { w: sr.right - sr.left, v, value, glyph, show_value, enabled, muted, mute, tones: *tn, hc: self.hc().map(|h| (h.text, h.highlight, h.highlight_text)), rtl: self.rtl },
        }
    }

    /// A slider's strip: the thick slider (a part), and beside the volume
    /// the output button (into the sound list).
    #[allow(clippy::too_many_arguments)]
    unsafe fn paint_slider(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, sliders: &mut Vec<(Hit, D2D_RECT_F)>, r: &Remote, tn: &Tones, hit: Hit, strip: D2D_RECT_F) -> Result<()> {
        self.card(dc, strip, tn)?;
        let (sr, button) = strip_slider(strip, hit == Hit::Volume);
        let spec = self.pill_spec(hit, sr, r, tn);
        if spec.look.enabled {
            if hit == Hit::Volume {
                hits.push((mute_in(sr), Hit::Mute));
            }
            hits.push((sr, hit));
            sliders.push((hit, sr));
            self.say_for(hit, &spec.look.value);
        } else {
            // No output device: no slider to carry it.
            self.say(hits, "text:no_output", t("control.no_output"), sr);
        }
        self.plan.borrow_mut().push(Spec::Pill(spec));
        self.ring(dc, hit, sr, self.theme.radii().control)?;
        if hit == Hit::Volume {
            self.ring(dc, Hit::Mute, mute_in(sr), MUTE / 2.0)?;
        }
        if let Some(b) = button {
            let list = device_list(r, Flow::Output);
            let cur = list.iter().find(|e| e.default);
            let name = match (cur, r.outputs.is_some(), list.is_empty()) {
                (Some(e), _, _) => e.name.clone(),
                (None, true, true) => t("control.no_output").to_string(),
                (None, true, false) => t("control.no_default").to_string(),
                (None, false, _) => String::new(),
            };
            let glyph = devices::glyph(cur.map(|e| e.kind).unwrap_or(devices::Kind::Speakers));
            hits.push((b, Hit::Outputs));
            // A disc on the strip's card, as the connections' chips are:
            // a raised tile is the card's own fill on Flat v2 and vanished
            // there (final audit 12).
            self.button(dc, tn, Hit::Outputs, b, ROUND_L / 2.0, glyph, TILE_GLYPH, Face::Chip, self.look(Hit::Outputs, false, true), true, false)?;
            self.tip(Hit::Outputs, if name.is_empty() { t("control.output").to_string() } else { format!("{} · {name}", t("control.output")) });
            self.say_for(Hit::Outputs, &name);
        }
        Ok(())
    }

    /// The position bar's look now (None: no timeline).
    pub(super) fn time_spec(&self, m: &Media, strip: D2D_RECT_F) -> Option<TimeSpec> {
        let tl = m.timeline.as_ref().filter(|tl| tl.shown())?;
        let plan = media_plan(strip, true, self.rtl);
        let pos = tl.position(std::time::Instant::now(), m.playing);
        Some(TimeSpec { r: plan.bar?, ratio: tl.ratio(pos), tones: self.tones(), hc: self.hc().map(|h| (h.text, h.highlight)) })
    }

    /// What is playing: the art (a part, cross-fading), the title and
    /// artist, the position bar (a part) under them, and the buttons
    /// (glyph buttons, parts).
    unsafe fn paint_media(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, r: &Remote, tn: &Tones, strip: D2D_RECT_F) -> Result<()> {
        let Some(m) = &r.media else { return Ok(()) };
        self.card(dc, strip, tn)?;
        let bar = m.timeline.as_ref().is_some_and(|tl| tl.shown());
        let mp = media_plan(strip, bar, self.rtl);
        let art_r = self.radius(Radius::Control);
        self.shadow(dc, mp.art, art_r, Elevation::E1)?;
        if m.art.is_none() {
            match self.hc() {
                Some(hc) => self.stroke_rr(dc, rgba(hc.text), mp.art, art_r, 1.0)?,
                None => self.fill_rr(dc, rgba(tn.tile_hover), mp.art, art_r)?,
            }
            self.glyph(dc, G_MUSIC, TILE_GLYPH, (mp.art.left + mp.art.right) / 2.0, (mp.art.top + mp.art.bottom) / 2.0, rgba(tn.dim))?;
        }
        self.plan.borrow_mut().push(Spec::Art(mp.art, m.art.clone()));
        let title = if m.title.is_empty() { t("control.now_playing").to_string() } else { m.title.clone() };
        let (x, w) = (mp.text_x, mp.text_w);
        self.line(dc, &title, T_NAME, true, x, mp.title_top, TITLE_H, w, rgba(tn.ink))?;
        self.line(dc, &m.artist, T_SMALL, false, x, mp.artist_top, ARTIST_H, w, rgba(tn.dim))?;
        // The whole title and artist, not cut to fit.
        self.say(hits, "text:media_title", &title, rect(x, mp.title_top, x + w, mp.title_top + TITLE_H));
        self.say(hits, "text:media_artist", &m.artist, rect(x, mp.artist_top, x + w, mp.artist_top + ARTIST_H));
        if let (Some(tl), Some(b)) = (m.timeline.as_ref().filter(|tl| tl.shown()), mp.bar) {
            let pos = tl.position(std::time::Instant::now(), m.playing);
            let (elapsed, total) = crate::topbar::island::model::time_pair(tl, pos);
            self.say(hits, "text:media_time", &format!("{elapsed} / {total}"), b);
        }
        // In their reading order (right to left they are placed turned).
        let mut buttons = mp.buttons;
        if self.rtl {
            buttons.reverse();
        }
        for (hit, br) in buttons {
            let enabled = match hit {
                Hit::Prev => m.prev,
                Hit::Next => m.next,
                _ => m.toggle,
            };
            let (glyph, gsize) = match hit {
                Hit::Prev => (G_PREV, MEDIA_GLYPH),
                Hit::Next => (G_NEXT, MEDIA_GLYPH),
                _ => (if m.playing { G_PAUSE } else { G_PLAY }, PLAY_GLYPH),
            };
            if enabled {
                hits.push((br, hit));
            }
            let d = br.right - br.left;
            self.button(dc, tn, hit, br, d / 2.0, glyph, gsize, Face::Glyph, self.look(hit, false, enabled), enabled, false)?;
        }
        if let Some(spec) = self.time_spec(m, strip) {
            self.plan.borrow_mut().push(Spec::Time(spec));
        }
        Ok(())
    }

    /// A removable device's strip: its glyph, name and second line (letters
    /// and labels, or the eject's note), and the eject button (a check once
    /// it is safe to remove).
    unsafe fn paint_eject(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, tn: &Tones, row: &crate::removable::model::Shown, strip: D2D_RECT_F) -> Result<()> {
        use crate::removable::model::Note;
        self.card(dc, strip, tn)?;
        let hit = Hit::Eject(removable_key(&row.device));
        let ready = eject_ready(&row.note);
        let cy = (strip.top + strip.bottom) / 2.0;
        let ink = rgba(tn.ink);
        let (gx, x, maxw, b) = eject_columns(strip);
        self.glyph(dc, removable_glyph(row.device.kind), TILE_GLYPH, gx, cy, ink)?;
        let note = removable_note(row);
        let name = if row.device.name.is_empty() { t("control.unknown_device") } else { row.device.name.as_str() };
        if note.is_empty() {
            self.line(dc, name, T_NAME, true, x, cy - TITLE_H / 2.0, TITLE_H, maxw, ink)?;
        } else {
            let (t1, t2) = two_lines(cy);
            self.line(dc, name, T_NAME, true, x, t1, TITLE_H, maxw, ink)?;
            self.line(dc, &note, T_SMALL, false, x, t2, ARTIST_H, maxw, rgba(tn.dim))?;
        }
        // On its way or done: no input, still read with its note (the
        // 2026-11-11 review: it left the reader's tree).
        add_control(hits, &mut self.inert.borrow_mut(), b, hit, ready);
        let glyph = if row.note == Note::Safe { G_CHECK } else { G_EJECT };
        self.button(dc, tn, hit, b, EJECT_BTN / 2.0, glyph, MEDIA_GLYPH, Face::Glyph, self.look(hit, false, ready), ready || row.note == Note::Safe, false)?;
        self.tip(hit, crate::removable::model::eject_name(t("control.eject.name_drives"), t("control.eject.name"), &row.device));
        // The whole line, not cut to fit.
        self.say_for(hit, &note);
        Ok(())
    }

    // ---- a list (Project's modes, the sound devices and apps) ----

    /// The list the panel went into: its header (the back button and the
    /// title) and its cards of rows.
    unsafe fn paint_list(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, sliders: &mut Vec<(Hit, D2D_RECT_F)>, r: &Remote, tn: &Tones, s: Section) -> Result<()> {
        let cards = section_cards(s, r);
        let (head, mut list) = sub_plan(&cards);
        if s.has_settings_footer() && self.scroll_max == 0.0 {
            if let Some((card, tops)) = list.last_mut() {
                let offset = (self.height - PAD - card.bottom).max(0.0);
                card.top += offset;
                card.bottom += offset;
                for top in tops {
                    *top += offset;
                }
            }
        }
        let body_top = self.list_body_top();
        let head = if body_top == 0.0 { shifted_y(head, -self.scroll) } else { head };
        let back = back_in(head);
        hits.push((back, Hit::Back));
        let arrow = if self.rtl { Glyph::ChevronRight } else { Glyph::ChevronLeft };
        self.button(dc, tn, Hit::Back, back, ROUND_S / 2.0, arrow, ROW_GLYPH, Face::Glyph, self.look(Hit::Back, false, true), true, false)?;
        let title = t(match s {
            Section::Project => "control.project",
            Section::Sound => "control.sound",
            Section::Wifi => "control.wifi",
            Section::Bluetooth => "control.bluetooth",
            Section::Accessibility => "control.accessibility",
        });
        // (Past the back button's focus ring.)
        let x = back.right + RING_OUT + S3;
        let scrolling = self.scroll_max > 0.0 && body_top > 0.0;
        let end = head.right - if scrolling { (ROUND_S + S2) * 2.0 } else { 0.0 };
        self.line(dc, title, T_HEAD, true, x, head.top, head.bottom - head.top, (end - x).max(0.0), rgba(tn.ink))?;
        self.say(hits, "text:list_title", title, rect(x, head.top, end, head.bottom));
        if scrolling {
            for (i, up) in [true, false].into_iter().enumerate() {
                let left = end + S2 + i as f32 * (ROUND_S + S2);
                let rc = rect(left, back.top, left + ROUND_S, back.bottom);
                let hit = Hit::ScrollList(up);
                let enabled = if up { self.scroll > 0.0 } else { self.scroll < self.scroll_max };
                let glyph = if up { Glyph::ChevronUp } else { Glyph::ChevronDown };
                add_control(hits, &mut self.inert.borrow_mut(), rc, hit, enabled);
                self.button(dc, tn, hit, rc, ROUND_S / 2.0, glyph, ROW_GLYPH, Face::Glyph, self.look(hit, false, enabled), enabled, false)?;
            }
        }
        dc.PushAxisAlignedClip(&rect(0.0, body_top, W, self.height), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        let result = (|| -> Result<()> {
            for (lines, (card, tops)) in cards.iter().zip(list) {
                let card = shifted_y(card, -self.scroll);
                self.card(dc, card, tn)?;
                let (l, rr) = (card.left + LIST_PAD, card.right - LIST_PAD);
                for (line, y) in lines.iter().zip(tops) {
                    let y = y - self.scroll;
                    let lr = rect(l, y, rr, y + line_h(*line));
                    match *line {
                        Line::Head(key) => {
                            self.line(dc, t(key), T_SMALL, false, l + ROW_IN, y, HEAD_H, rr - l - 2.0 * ROW_IN, rgba(tn.faint))?;
                            // Read where it is drawn: its rows start under it.
                            self.say(hits, head_node(key), t(key), lr);
                        }
                        Line::Mode(m) => {
                            let current = r.displays == Displays::Many(Some(m));
                            self.list_row(dc, hits, tn, Hit::Mode(m), lr, Some(Glyph::Project), t(m.label()), current, None)?;
                        }
                        Line::Device(flow, k) => {
                            let Some(e) = device_list(r, flow).iter().find(|e| device_key(&e.id) == k) else { continue };
                            // Asked of Windows, not answered yet.
                            let busy = r.audio_busy.as_ref().is_some_and(|(f, id)| *f == flow && *id == e.id);
                            let note = busy.then(|| t("control.audio.switching"));
                            self.list_row(dc, hits, tn, Hit::Device(flow, k), lr, Some(devices::glyph(e.kind)), &e.name, e.default && !busy, note)?;
                        }
                        Line::Failed(flow) => {
                            let name = fail_for(r, flow).unwrap_or("");
                            let msg = if name.is_empty() { t("control.audio.gone").to_string() } else { tf("control.audio.failed", &[("name", name)]) };
                            let cy = (lr.top + lr.bottom) / 2.0;
                            let x = l + ROW_IN + ROW_GLYPH + S3;
                            self.glyph(dc, G_INFO, ROW_GLYPH, l + ROW_IN + ROW_GLYPH / 2.0, cy, rgba(tn.ink))?;
                            self.line(dc, &msg, T_SMALL, false, x, lr.top, LINE_H, (rr - ROW_IN - x).max(0.0), rgba(tn.ink))?;
                            self.say(hits, "text:audio_failed", &msg, lr);
                        }
                        Line::SoundSettings => {
                            // A name, not a sentence ("Sound settings…").
                            self.list_row(dc, hits, tn, Hit::SoundSettings, lr, Some(G_EXTERNAL), t("topbar.panel.volume.settings"), false, None)?;
                        }
                        Line::App(k) => self.app_row(dc, hits, sliders, r, tn, k, lr)?,
                        Line::Control(hit) => self.paint_list_control(dc, hits, r, tn, hit, lr)?,
                        Line::Status(key) => {
                            let status = t(key);
                            self.line(dc, status, T_SMALL, false, l + ROW_IN, y, LINE_H, rr - l - 2.0 * ROW_IN, rgba(tn.dim))?;
                            self.say(hits, "text:network_status", status, lr);
                        }
                    }
                }
            }
            Ok(())
        })();
        dc.PopAxisAlignedClip();
        result
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn wireless_row(
        &self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, tn: &Tones,
        hit: Hit, rc: D2D_RECT_F, glyphs: &[Glyph], name: &str, state: &str, end: Option<String>, active: bool,
    ) -> Result<()> {
        let look = self.look(hit, false, active);
        let ink = self.row_face(dc, tn, rc, look.hover, look.pressed)?;
        let dim = if self.hc().is_some() { ink } else { rgba(tn.dim) };
        let cy = (rc.top + rc.bottom) / 2.0;
        let mut x = rc.left + ROW_IN;
        for &glyph in glyphs {
            self.glyph(dc, glyph, ROW_GLYPH, x + ROW_GLYPH / 2.0, cy, dim)?;
            x += ROW_GLYPH + S2;
        }
        if matches!(hit, Hit::WifiRow(_)) {
            x = rc.left + ROW_IN + 2.0 * (ROW_GLYPH + S2);
        }
        x += 4.0;
        let edge = rc.right - ROW_IN;
        let mut max_w = edge - x;
        if let Some(end) = &end {
            max_w -= 8.0 + self.line_end(dc, end, T_SMALL, edge, rc.top, rc.bottom - rc.top, 48.0, dim)?;
        }
        if state.is_empty() {
            self.line(dc, name, T_BODY, false, x, rc.top, rc.bottom - rc.top, max_w, ink)?;
        } else {
            let (top, detail) = two_lines(cy);
            self.line(dc, name, T_BODY, false, x, top, TITLE_H, max_w, ink)?;
            self.line(dc, state, T_SMALL, false, x, detail, ARTIST_H, max_w, dim)?;
        }
        add_control(hits, &mut self.inert.borrow_mut(), rc, hit, active);
        let value = match end {
            Some(battery) => format!("{state}, {battery}"),
            None => state.to_string(),
        };
        self.say_for(hit, &value);
        self.ring(dc, hit, rc, TILE_CORNER)
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn paint_list_control(
        &self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, r: &Remote, tn: &Tones, hit: Hit, rc: D2D_RECT_F,
    ) -> Result<()> {
        let now = Instant::now();
        let key = match hit {
            Hit::WifiRow(key) => {
                let Some(n) = wifi_network(r, key) else { return Ok(()) };
                let note = switch_note(r.wifi_busy.is_some_and(|(k, _)| k == key), r.wifi_failed.filter(|(k, _)| *k == key).map(|(_, at)| at), now);
                let state = note.map(t).unwrap_or_else(|| if n.connected { t("control.connected") } else { "" });
                let glyphs = super::super::panels::wifi_trail(n.signal, n.secured);
                if n.connected {
                    let end = rc.right - 4.0;
                    let button = rect(end - 116.0, rc.top + 6.0, end, rc.bottom - 6.0);
                    let info = rect(rc.left, rc.top, button.left, rc.bottom);
                    self.wireless_row(dc, hits, tn, hit, info, &glyphs, &n.ssid, state, None, false)?;
                    let action = Hit::WifiDisconnect(key);
                    let enabled = r.wifi_busy.is_none();
                    let look = self.look(action, false, enabled);
                    let (fill, ink) = chip_tone(tn, look);
                    let ink = if let Some(hc) = self.hc() {
                        self.stroke_rr(dc, rgba(hc.text), button, TILE_CORNER, 1.0)?;
                        hc.text
                    } else {
                        self.fill_rr(dc, rgba(fill), button, TILE_CORNER)?;
                        ink
                    };
                    add_control(hits, &mut self.inert.borrow_mut(), button, action, enabled);
                    if let Some((l, w, h)) = self.fit_text(t("control.disconnect"), T_SMALL, false, 108.0) {
                        self.draw_text(dc, &l, (button.left + button.right - w) / 2.0,
                            (button.top + button.bottom - h) / 2.0, rgba(ink))?;
                    }
                    return self.ring(dc, action, button, TILE_CORNER);
                }
                self.tip(hit, t("control.connect").to_string());
                return self.wireless_row(dc, hits, tn, hit, rc, &glyphs, &n.ssid, state, None, r.wifi_busy.is_none());
            }
            Hit::BtDevice(key) => {
                let Some(d) = bluetooth_device(r, key) else { return Ok(()) };
                let note = switch_note(r.bt_busy.contains(&key), r.bt_failed.get(&key).copied(), now);
                let connected = d.connected && r.bt.on();
                let state = t(note.unwrap_or(if connected { "control.connected" } else { "control.not_connected" }));
                let end = d.battery.map(|n| tf("control.percent", &[("p", &n.to_string())]));
                self.tip(hit, t(if d.can_switch && r.bt.on() {
                    if connected { "control.disconnect" } else { "control.connect" }
                } else { "control.bluetooth_settings" }).to_string());
                let ready = d.kind != BtKind::Audio || !r.bt.on() || r.bt_audio_ready;
                return self.wireless_row(dc, hits, tn, hit, rc, &[d.kind.glyph()], &d.name, state, end, ready);
            }
            Hit::Access(a) => {
                let view = r.access[a as usize];
                let note = switch_note(view.busy, view.failed, now).map(t);
                if a == Access::MonoAudio || view.on.is_none() {
                    let state = view.on.map(|on| t(if on { "control.on" } else { "control.off" }));
                    self.say_for(hit, state.unwrap_or(""));
                    return self.list_row(dc, hits, tn, hit, rc, Some(a.glyph()), t(a.label()), false, state);
                }
                return self.access_row(dc, hits, tn, a, rc, view, note);
            }
            Hit::BluetoothSettings => "control.bluetooth_settings",
            Hit::AccessibilitySettings => "control.accessibility_settings",
            Hit::WifiSettings => "control.wifi_settings",
            Hit::WifiRefresh => "launch.action.retry",
            Hit::WifiLocation => "control.settings",
            _ => return Ok(()),
        };
        let glyph = if hit == Hit::WifiRefresh { Glyph::Restart } else { G_EXTERNAL };
        self.list_row(dc, hits, tn, hit, rc, Some(glyph), t(key), false, None)
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn access_row(
        &self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, tn: &Tones,
        access: Access, rc: D2D_RECT_F, view: AccessView, note: Option<&str>,
    ) -> Result<()> {
        let hit = Hit::Access(access);
        let on = view.on == Some(true);
        let look = self.look(hit, on, !view.busy);
        let ink = self.row_face(dc, tn, rc, look.hover, look.pressed)?;
        let cy = (rc.top + rc.bottom) / 2.0;
        let x = rc.left + ROW_IN;
        let end = rc.right - ROW_IN;
        let (w, h) = (crate::gfx::parts::SWITCH_W, crate::gfx::parts::SWITCH_H);
        let track = rect(end - w, cy - h / 2.0, end, cy + h / 2.0);
        let (fill, knob) = match self.hc() {
            Some(hc) if on => (hc.highlight, hc.highlight_text),
            Some(hc) => (hc.window, hc.text),
            None if on => raised_tone(tn, look),
            None => (tn.groove, self.theme.knob()),
        };
        self.fill_rr(dc, rgba(fill), track, h / 2.0)?;
        if let Some(hc) = self.hc() {
            self.stroke_rr(dc, rgba(hc.text), track, h / 2.0, 1.0)?;
        }
        let kx = if on { track.right - h / 2.0 } else { track.left + h / 2.0 };
        let rad = h / 2.0 - 3.0;
        self.fill_rr(dc, rgba(knob), rect(kx - rad, cy - rad, kx + rad, cy + rad), rad)?;
        self.glyph(dc, access.glyph(), ROW_GLYPH, x + ROW_GLYPH / 2.0, cy, ink)?;
        let name_x = x + ROW_GLYPH + S3;
        let mut room = track.left - 8.0 - name_x;
        if let Some(n) = note {
            let used = self.line_end(dc, n, T_SMALL, track.left - 8.0, rc.top, rc.bottom - rc.top, 90.0, ink)?;
            room -= used + 8.0;
            self.say_for(hit, n);
        }
        self.line(dc, t(access.label()), T_BODY, false, name_x, rc.top, rc.bottom - rc.top, room, ink)?;
        hits.push((rc, hit));
        self.ring(dc, hit, rc, TILE_CORNER)
    }

    /// A row in a list: its glyph, its text, and a check when it is the
    /// one in use, or a note at its end ("Switching…").
    #[allow(clippy::too_many_arguments)]
    unsafe fn list_row(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, tn: &Tones, hit: Hit, lr: D2D_RECT_F, glyph: Option<Glyph>, text: &str, selected: bool, note: Option<&str>) -> Result<()> {
        let l = self.look(hit, false, true);
        let ink = self.row_face(dc, tn, lr, l.hover, l.pressed)?;
        let dim = if self.hc().is_some() { ink } else { rgba(tn.dim) };
        hits.push((lr, hit));
        let cy = (lr.top + lr.bottom) / 2.0;
        let mut x = lr.left + ROW_IN;
        if let Some(g) = glyph {
            self.glyph(dc, g, ROW_GLYPH, x + ROW_GLYPH / 2.0, cy, dim)?;
            x += ROW_GLYPH + S3;
        }
        let mut end = lr.right - ROW_IN;
        if let Some(n) = note {
            let w = self.line_end(dc, n, T_SMALL, end, lr.top, lr.bottom - lr.top, NOTE_MAX, dim)?;
            end -= w + S3;
        } else if selected {
            self.glyph(dc, G_CHECK, ROW_GLYPH, end - ROW_GLYPH / 2.0, cy, ink)?;
            end -= ROW_GLYPH + S3;
        }
        self.line(dc, text, T_BODY, false, x, lr.top, lr.bottom - lr.top, (end - x).max(0.0), ink)?;
        self.ring(dc, hit, lr, self.radius(Radius::Row))
    }

    /// An app's volume now (the dragged value while it is dragged).
    pub(super) fn app_level(&self, k: u64, level: u8) -> u8 {
        match self.app_drag {
            Some((dk, p)) if dk == k => p,
            _ => level,
        }
    }

    /// An app's volume row: its icon and name, a slim slider (its knob
    /// grows while held) and its percent.
    #[allow(clippy::too_many_arguments)]
    unsafe fn app_row(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, sliders: &mut Vec<(Hit, D2D_RECT_F)>, r: &Remote, tn: &Tones, k: u64, lr: D2D_RECT_F) -> Result<()> {
        let Some(a) = r.apps.as_deref().unwrap_or(&[]).iter().find(|a| a.key == k) else { return Ok(()) };
        let hit = Hit::App(k);
        let level = self.app_level(k, a.level);
        let cy = (lr.top + lr.bottom) / 2.0;
        let ink = rgba(tn.ink);
        let c = app_columns(lr);
        let mut nx = c.name;
        if let Some(icon) = &a.icon {
            self.app_icon(dc, icon, rect(nx, cy - ROW_GLYPH / 2.0, nx + ROW_GLYPH, cy + ROW_GLYPH / 2.0))?;
            nx += ROW_GLYPH + S3;
        }
        self.line(dc, &a.name, T_BODY, false, nx, lr.top, lr.bottom - lr.top, (c.name + c.name_w - nx).max(0.0), ink)?;
        let (x0, x1) = c.track;
        let v = level as f32 / 100.0;
        let kx = x0 + (x1 - x0) * v;
        let half = TRACK_H / 2.0;
        let track = rect(x0, cy - half, x1, cy + half);
        let played = rect(x0, cy - half, kx.max(x0 + TRACK_H), cy + half);
        let cap = crate::theme::capsule(TRACK_H);
        match self.hc() {
            Some(hc) => {
                self.stroke_rr(dc, rgba(hc.text), rect(x0, cy - half - 0.5, x1, cy + half + 0.5), cap + 0.5, 1.0)?;
                self.fill_rr(dc, rgba(hc.highlight), played, cap)?;
            }
            None => {
                self.fill_rr(dc, rgba(tn.groove), track, cap)?;
                self.fill_rr(dc, rgba(tn.fill), played, cap)?;
            }
        }
        let held = self.sliding == Some(hit);
        let rad = KNOB / 2.0 * if held && self.moving() { KNOB_HELD } else { 1.0 };
        let knob = rect(kx - rad, cy - rad, kx + rad, cy + rad);
        // (Glass A: one soft layer, Theme::knob_shadow; Flat keeps E1.)
        if self.theme.glass() {
            if self.hc().is_none() && crate::effects::items().shadows {
                crate::gfx::soft_shadows(dc, knob, rad, &self.theme.knob_shadow())?;
            }
        } else {
            self.shadow(dc, knob, rad, Elevation::E1)?;
        }
        let thumb = match self.hc() {
            Some(hc) => rgba(hc.text),
            None => rgba(self.theme.knob()),
        };
        let b = dc.CreateSolidColorBrush(&thumb, None)?;
        dc.FillEllipse(&D2D1_ELLIPSE { point: Vector2 { X: kx, Y: cy }, radiusX: rad, radiusY: rad }, &b);
        let said = if a.muted { t("control.muted").to_string() } else { tf("control.percent", &[("p", &level.to_string())]) };
        if let Some((l, w, h)) = self.tabular(&said, T_CAPTION, false) {
            self.draw_text(dc, &l, c.value_end - w, cy - h / 2.0, if self.hc().is_some() { ink } else { rgba(tn.dim) })?;
        }
        self.say_for(hit, &said);
        hits.push((rect(x0 - KNOB / 2.0, lr.top, x1 + KNOB / 2.0, lr.bottom), hit));
        sliders.push((hit, track));
        // The ring goes round the knob (6.2).
        self.ring(dc, hit, knob, rad)
    }
}
