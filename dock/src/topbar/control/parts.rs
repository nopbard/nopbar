//! The animated parts: visuals of their own over the drawn surface (the
//! buttons, the thick sliders, the album art and the position bar).

use super::*;

impl Panel {
    /// The visual a view's parts go in.
    pub(super) fn host(&self, sub: bool) -> &ContainerVisual {
        if sub { &self.v.parts_sub } else { &self.v.parts_main }
    }

    /// A white mask (a shape, a glyph or the mark) `px` a side, made once;
    /// or a glyph's halo in its own color (MaskKey::Halo).
    pub(super) fn mask(&self, parts: &mut Parts, key: MaskKey, d: f32) -> Result<CompositionSurfaceBrush> {
        if let Some((_, b)) = parts.masks.iter().find(|m| m.0 == key) {
            return Ok(b.clone());
        }
        let ds = self.ds();
        let px = (d * ds).ceil().max(1.0) as u32;
        let white = D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 };
        let surface = self.gfx.draw(px, px, |dc, ox, oy| unsafe { paint_shape(dc, ox, oy, key, d, ds, px, white) })?;
        let b = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface)?;
        b.SetStretch(CompositionStretch::Fill)?;
        parts.masks.push((key, b.clone()));
        Ok(b)
    }

    /// A part's left edge as placed (right to left: mirrored).
    pub(super) fn placed_x(&self, left: f32, w: f32) -> f32 {
        crate::i18n::mirror::span(self.rtl, left, w, 0.0, W)
    }

    /// A part's top left (its box `left`, `top`, `w` wide) as placed:
    /// mirrored right to left, on whole device pixels (snap_dip). A part
    /// on a half pixel (an odd DIP at 150 %) was resampled soft by the
    /// compositor, the thick sliders' edge doubled (states-issues 2).
    pub(super) fn placed(&self, left: f32, top: f32, w: f32) -> (f32, f32) {
        (snap_dip(self.placed_x(left, w), self.ds()), snap_dip(top, self.ds()))
    }

    /// The mask of a button's shape (filled, or outlined in high contrast).
    fn shape_key(&self, s: &DiscSpec) -> MaskKey {
        let ds = self.ds();
        let d = s.r.right - s.r.left;
        let px = (d * ds).ceil() as u32;
        let k = px as f32 / d;
        MaskKey::Shape(px, (s.rad * k * 100.0).round() as u32, s.ring.map_or(0, |w| (w * k * 100.0).round() as u32))
    }

    /// The mask of a raised tile's top edge light (Flat v2's card light).
    fn light_key(&self, s: &DiscSpec) -> MaskKey {
        let ds = self.ds();
        let d = s.r.right - s.r.left;
        let px = (d * ds).ceil() as u32;
        let k = px as f32 / d;
        MaskKey::Light(px, (s.rad * k * 100.0).round() as u32, (k * 100.0).round() as u32)
    }

    pub(super) fn make_disc(&self, parts: &mut Parts, s: &DiscSpec) -> Result<DiscPart> {
        let c = &self.gfx.compositor;
        let d = s.r.right - s.r.left;
        let px = (d * self.ds()).ceil() as u32;
        let vis = c.CreateSpriteVisual()?;
        vis.SetSize(Vector2 { X: d, Y: d })?;
        vis.SetCenterPoint(Vector3 { X: d / 2.0, Y: d / 2.0, Z: 0.0 })?;
        let child = |brush: &windows::UI::Composition::CompositionMaskBrush, y: f32| -> Result<SpriteVisual> {
            let v = c.CreateSpriteVisual()?;
            v.SetSize(Vector2 { X: d, Y: d })?;
            v.SetOffset(Vector3 { X: 0.0, Y: y, Z: 0.0 })?;
            v.SetBrush(brush)?;
            vis.Children()?.InsertAtTop(&v)?;
            Ok(v)
        };
        // Flat v2's drop (half a DIP down, its shape), the face, the top
        // edge's light (DiscSpec::lit).
        let drop = c.CreateColorBrushWithColor(ui_color(Rgba { r: 0, g: 0, b: 0, a: s.lit.1 }))?;
        let dmask = c.CreateMaskBrush()?;
        dmask.SetSource(&drop)?;
        dmask.SetMask(&self.mask(parts, self.shape_key(s), d)?)?;
        let drop_v = child(&dmask, 0.5)?;
        let fill = c.CreateColorBrushWithColor(ui_color(s.fill))?;
        let mask = c.CreateMaskBrush()?;
        mask.SetSource(&fill)?;
        mask.SetMask(&self.mask(parts, self.shape_key(s), d)?)?;
        let face_v = child(&mask, 0.0)?;
        let light = c.CreateColorBrushWithColor(ui_color(Rgba { r: 255, g: 255, b: 255, a: s.lit.0 }))?;
        let lmask = c.CreateMaskBrush()?;
        lmask.SetSource(&light)?;
        lmask.SetMask(&self.mask(parts, self.light_key(s), d)?)?;
        let light_v = child(&lmask, 0.0)?;
        let ink = c.CreateColorBrushWithColor(ui_color(s.ink))?;
        let gmask = c.CreateMaskBrush()?;
        gmask.SetSource(&ink)?;
        gmask.SetMask(&self.mask(parts, MaskKey::Glyph(s.glyph, px, (s.gsize * 100.0) as u32), d)?)?;
        let glyph_v = c.CreateSpriteVisual()?;
        glyph_v.SetSize(Vector2 { X: d, Y: d })?;
        glyph_v.SetBrush(&gmask)?;
        glyph_v.SetOpacity(if s.dim { DIM } else { 1.0 })?;
        vis.Children()?.InsertAtTop(&glyph_v)?;
        let mark_v = if s.mark { Some(self.make_mark(parts, &vis, &ink, d, px)?) } else { None };
        let (x, y) = self.placed(s.r.left, s.r.top, d);
        vis.SetOffset(Vector3 { X: x, Y: y, Z: 0.0 })?;
        self.host(parts.sub).Children()?.InsertAtTop(&vis)?;
        Ok(DiscPart { spec: *s, vis, glyph_v, mark_v, face_v, drop_v, light_v, drop, light, dmask, lmask, fill, ink, mask, gmask })
    }

    /// The "opens elsewhere" mark on a tile: the glyph's ink, fainter.
    fn make_mark(&self, parts: &mut Parts, vis: &SpriteVisual, ink: &CompositionColorBrush, d: f32, px: u32) -> Result<SpriteVisual> {
        let c = &self.gfx.compositor;
        let m = c.CreateMaskBrush()?;
        m.SetSource(ink)?;
        m.SetMask(&self.mask(parts, MaskKey::Mark(px), d)?)?;
        let v = c.CreateSpriteVisual()?;
        v.SetSize(Vector2 { X: d, Y: d })?;
        v.SetBrush(&m)?;
        v.SetOpacity(MARK_ALPHA)?;
        vis.Children()?.InsertAtTop(&v)?;
        Ok(v)
    }

    pub(super) fn update_disc(&self, parts: &mut Parts, p: &mut DiscPart, s: &DiscSpec) -> Result<()> {
        let c = &self.gfx.compositor;
        let was = p.spec;
        if was == *s {
            return Ok(());
        }
        let d = s.r.right - s.r.left;
        let px = (d * self.ds()).ceil() as u32;
        if was.r != s.r {
            p.vis.SetSize(Vector2 { X: d, Y: d })?;
            p.vis.SetCenterPoint(Vector3 { X: d / 2.0, Y: d / 2.0, Z: 0.0 })?;
            p.glyph_v.SetSize(Vector2 { X: d, Y: d })?;
            for v in [&p.face_v, &p.drop_v, &p.light_v] {
                v.SetSize(Vector2 { X: d, Y: d })?;
            }
            let (x, y) = self.placed(s.r.left, s.r.top, d);
            p.vis.SetOffset(Vector3 { X: x, Y: y, Z: 0.0 })?;
        }
        // High contrast's fills change at once (a fade's middle colors
        // would break its contrast).
        let (ms, e) = if self.hc().is_some() { (0, EASE_OUT) } else { tone_ms(was.look, s.look) };
        if was.fill != s.fill {
            tint(c, &p.fill, s.fill, ms, e)?;
        }
        if was.ink != s.ink {
            tint(c, &p.ink, s.ink, ms, e)?;
        }
        if was.ring != s.ring || was.rad != s.rad || was.r != s.r {
            p.mask.SetMask(&self.mask(parts, self.shape_key(s), d)?)?;
            p.dmask.SetMask(&self.mask(parts, self.shape_key(s), d)?)?;
            p.lmask.SetMask(&self.mask(parts, self.light_key(s), d)?)?;
        }
        if was.lit.0 != s.lit.0 {
            // The light goes at once pressed, comes back with the fill.
            tint(c, &p.light, Rgba { r: 255, g: 255, b: 255, a: s.lit.0 }, if s.look.pressed { 0 } else { ms }, e)?;
        }
        if was.lit.1 != s.lit.1 {
            tint(c, &p.drop, Rgba { r: 0, g: 0, b: 0, a: s.lit.1 }, ms, e)?;
        }
        if was.glyph != s.glyph || was.gsize != s.gsize || was.r != s.r {
            p.gmask.SetMask(&self.mask(parts, MaskKey::Glyph(s.glyph, px, (s.gsize * 100.0) as u32), d)?)?;
        }
        if was.dim != s.dim {
            p.glyph_v.SetOpacity(if s.dim { DIM } else { 1.0 })?;
        }
        if was.mark != s.mark || (s.mark && was.r != s.r) {
            if let Some(m) = p.mark_v.take() {
                let _ = p.vis.Children().and_then(|ch| ch.Remove(&m));
            }
            if s.mark {
                p.mark_v = Some(self.make_mark(parts, &p.vis, &p.ink, d, px)?);
            }
        }
        if was.look.pressed != s.look.pressed && self.moving() {
            // Pressed: at once smaller (the fill already darker); let go:
            // back on the SNAP spring.
            let o = obj(&p.vis)?;
            if s.look.pressed {
                let _ = o.StopAnimation(&hs("Scale"));
                p.vis.SetScale(Vector3 { X: s.press, Y: s.press, Z: 1.0 })?;
            } else {
                spring_to(c, &o, "Scale", "Vector3(1, 1, 1)", SPRING_A, true)?;
            }
        }
        p.spec = *s;
        Ok(())
    }

    /// The slider's full-height groove and fill, with the glyph inside the fill.
    pub(super) unsafe fn draw_pill(&self, dc: &ID2D1DeviceContext, k: &PillLook) -> Result<()> {
        let (w, h) = (k.w, CAPSULE);
        let rad = self.theme.radii().control;
        let all = rect(0.0, 0.0, w, h);
        let mirror = |r: D2D_RECT_F| {
            let (left, right) = crate::i18n::mirror::edges(k.rtl, r.left, r.right, 0.0, w);
            rect(left, r.top, right, r.bottom)
        };
        let (ink, fill, fill_ink) = match k.hc {
            Some((text, highlight, highlight_text)) => {
                self.stroke_rr(dc, rgba(text), all, rad, 1.0)?;
                (text, highlight, highlight_text)
            }
            None => {
                self.fill_rr(dc, rgba(k.tones.groove), all, rad)?;
                (k.tones.dim, Rgba { a: if self.theme.is_light { 255 } else { 235 }, ..WHITE }, BLACK)
            }
        };
        let fill_width = h + (w - h).max(0.0) * k.v.clamp(0.0, 1.0);
        let filled = mirror(rect(0.0, 0.0, fill_width, h));
        let highlighted = k.enabled && !k.muted;
        if k.enabled {
            let mut color = rgba(fill);
            if k.muted {
                color.a *= 0.35;
            }
            self.fill_rr(dc, color, filled, rad)?;
        }
        let mute = mute_in(all);
        if let Some((hover, pressed)) = k.mute {
            if hover || pressed {
                let button = mirror(mute);
                if let Some((_, highlight, highlight_text)) = k.hc {
                    self.stroke_rr(dc, rgba(if highlighted { highlight_text } else { highlight }), button, MUTE / 2.0, 2.0)?;
                } else {
                    self.fill_rr(dc, rgba(Rgba { a: if pressed { 26 } else { 15 }, ..BLACK }), button, MUTE / 2.0)?;
                }
            }
        }
        let glyph_x = crate::i18n::mirror::x(k.rtl, (mute.left + mute.right) / 2.0, 0.0, w);
        let mut glyph_ink = rgba(if highlighted { fill_ink } else { ink });
        if !k.enabled {
            glyph_ink.a *= DIM;
        }
        self.glyph(dc, k.glyph, CAP_GLYPH, glyph_x, h / 2.0, glyph_ink)?;
        if k.show_value {
            if let Some((layout, width, height)) = self.tabular(&k.value, T_SMALL, false) {
                let x = crate::i18n::mirror::span(k.rtl, w - STRIP_IN - width, width, 0.0, w);
                let over_fill = highlighted && fill_width >= w - STRIP_IN - width / 2.0;
                self.draw_plain(dc, &layout, x, (h - height) / 2.0, rgba(if over_fill { fill_ink } else { ink }))?;
            }
        }
        Ok(())
    }

    pub(super) fn pill_surface(&self, k: &PillLook) -> Result<CompositionSurfaceBrush> {
        let ds = self.ds();
        let (pw, ph) = ((k.w * ds).ceil() as u32, (CAPSULE * ds).ceil() as u32);
        let surface = self.gfx.draw(pw, ph, |dc, ox, oy| unsafe {
            dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            dc.SetTransform(&Matrix3x2 { M11: ds, M12: 0.0, M21: 0.0, M22: ds, M31: ox, M32: oy });
            let r = self.draw_pill(dc, k);
            dc.SetTransform(&Matrix3x2::identity());
            r
        })?;
        let b = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface)?;
        b.SetStretch(CompositionStretch::Fill)?;
        Ok(b)
    }

    pub(super) fn make_pill(&self, parts: &Parts, s: &PillSpec) -> Result<PillPart> {
        let c = &self.gfx.compositor;
        let vis = c.CreateSpriteVisual()?;
        let w = s.r.right - s.r.left;
        vis.SetSize(Vector2 { X: w, Y: CAPSULE })?;
        vis.SetCenterPoint(Vector3 { X: w / 2.0, Y: CAPSULE / 2.0, Z: 0.0 })?;
        vis.SetBrush(&self.pill_surface(&s.look)?)?;
        let (x, y) = self.placed(s.r.left, s.r.top, w);
        vis.SetOffset(Vector3 { X: x, Y: y, Z: 0.0 })?;
        self.host(parts.sub).Children()?.InsertAtTop(&vis)?;
        Ok(PillPart { spec: s.clone(), vis })
    }

    pub(super) fn update_pill(&self, p: &mut PillPart, s: &PillSpec) -> Result<()> {
        if p.spec.look != s.look {
            p.vis.SetBrush(&self.pill_surface(&s.look)?)?;
        }
        if p.spec.r != s.r {
            let w = s.r.right - s.r.left;
            p.vis.SetSize(Vector2 { X: w, Y: CAPSULE })?;
            p.vis.SetCenterPoint(Vector3 { X: w / 2.0, Y: CAPSULE / 2.0, Z: 0.0 })?;
            let (x, y) = self.placed(s.r.left, s.r.top, w);
            p.vis.SetOffset(Vector3 { X: x, Y: y, Z: 0.0 })?;
        }
        if p.spec.held != s.held {
            // Held: a little thicker (SNAP), back on letting go. Reduced
            // motion: nothing grows.
            let c = &self.gfx.compositor;
            let o = obj(&p.vis)?;
            if self.moving() {
                let y = if s.held { HELD_GROW } else { 1.0 };
                spring_to(c, &o, "Scale", &format!("Vector3(1, {y}, 1)"), SPRING_A, true)?;
            } else {
                let _ = o.StopAnimation(&hs("Scale"));
                p.vis.SetScale(Vector3 { X: 1.0, Y: 1.0, Z: 1.0 })?;
            }
        }
        p.spec = s.clone();
        Ok(())
    }

    pub(super) fn make_art(&self, parts: &Parts) -> Result<ArtPart> {
        let c = &self.gfx.compositor;
        let vis = c.CreateContainerVisual()?;
        vis.SetSize(Vector2 { X: ART_DIP, Y: ART_DIP })?;
        let geom = c.CreateRoundedRectangleGeometry()?;
        geom.SetSize(Vector2 { X: ART_DIP, Y: ART_DIP })?;
        let r = self.radius(Radius::Control);
        geom.SetCornerRadius(Vector2 { X: r, Y: r })?;
        vis.SetClip(&c.CreateGeometricClipWithGeometry(&geom)?)?;
        let a = c.CreateSpriteVisual()?;
        let b = c.CreateSpriteVisual()?;
        for s in [&a, &b] {
            s.SetSize(Vector2 { X: ART_DIP, Y: ART_DIP })?;
            s.SetOpacity(0.0)?;
            vis.Children()?.InsertAtTop(s)?;
        }
        self.host(parts.sub).Children()?.InsertAtTop(&vis)?;
        Ok(ArtPart { vis, sprites: [a, b], front: 0, art: None })
    }

    /// The art in its place; a new picture cross-fades over the old one
    /// (inside the art's own bounds), none fades the old one out.
    pub(super) fn update_art(&self, p: &mut ArtPart, r: D2D_RECT_F, art: Option<&Arc<Art>>, first: bool) -> Result<()> {
        let (x, y) = self.placed(r.left, r.top, ART_DIP);
        p.vis.SetOffset(Vector3 { X: x, Y: y, Z: 0.0 })?;
        let same = match (art, &p.art) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if same {
            return Ok(());
        }
        let c = &self.gfx.compositor;
        let front = obj(&p.sprites[p.front])?;
        match art {
            Some(a) => {
                let px = (ART_DIP * self.ds()).ceil() as u32;
                let surface = self.gfx.surface_fit(a.px, a.px, &a.pixels, px, false)?;
                let brush = c.CreateSurfaceBrushWithSurface(&surface)?;
                brush.SetStretch(CompositionStretch::Fill)?;
                let back = 1 - p.front;
                let bv = &p.sprites[back];
                bv.SetBrush(&brush)?;
                if first {
                    bv.SetOpacity(1.0)?;
                    p.sprites[p.front].SetOpacity(0.0)?;
                } else {
                    bv.SetOpacity(0.0)?;
                    fade_scalar(c, &obj(bv)?, "Opacity", 1.0, ART_FADE_MS, EASE_OUT)?;
                    fade_scalar(c, &front, "Opacity", 0.0, ART_FADE_MS, EASE_OUT)?;
                }
                // The newer picture on top.
                p.vis.Children()?.Remove(bv)?;
                p.vis.Children()?.InsertAtTop(bv)?;
                p.front = back;
            }
            None => fade_scalar(c, &front, "Opacity", 0.0, ART_FADE_MS, EASE_OUT)?,
        }
        p.art = art.cloned();
        Ok(())
    }

    /// The position bar: its groove and the played part (right to left:
    /// from the right).
    pub(super) unsafe fn draw_time(&self, dc: &ID2D1DeviceContext, s: &TimeSpec) -> Result<()> {
        let w = s.r.right - s.r.left;
        let tn = &s.tones;
        let fw = w * s.ratio.clamp(0.0, 1.0);
        let (x0, x1) = if self.rtl { (w - fw, w) } else { (0.0, fw) };
        let cap = crate::theme::capsule(TRACK_H);
        match s.hc {
            Some((text, hl)) => {
                self.stroke_rr(dc, rgba(text), rect(0.0, -0.5, w, TRACK_H + 0.5), cap + 0.5, 1.0)?;
                self.fill_rr(dc, rgba(hl), rect(x0, 0.0, x1, TRACK_H), cap)?;
            }
            None => {
                self.fill_rr(dc, rgba(tn.bar), rect(0.0, 0.0, w, TRACK_H), cap)?;
                self.fill_rr(dc, rgba(tn.fill), rect(x0, 0.0, x1, TRACK_H), cap)?;
            }
        }
        Ok(())
    }

    pub(super) fn time_surface(&self, s: &TimeSpec) -> Result<CompositionSurfaceBrush> {
        let ds = self.ds();
        let (pw, ph) = (((s.r.right - s.r.left) * ds).ceil() as u32, (TRACK_H * ds).ceil() as u32);
        let surface = self.gfx.draw(pw, ph, |dc, ox, oy| unsafe {
            dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            dc.SetTransform(&Matrix3x2 { M11: ds, M12: 0.0, M21: 0.0, M22: ds, M31: ox, M32: oy });
            let r = self.draw_time(dc, s);
            dc.SetTransform(&Matrix3x2::identity());
            r
        })?;
        let b = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface)?;
        b.SetStretch(CompositionStretch::Fill)?;
        Ok(b)
    }

    pub(super) fn sync_time(&self, parts: &mut Parts, s: Option<&TimeSpec>) -> Result<()> {
        let host = self.host(parts.sub);
        let Some(s) = s else {
            if let Some(p) = parts.time.take() {
                let _ = host.Children().and_then(|c| c.Remove(&p.vis));
            }
            return Ok(());
        };
        if parts.time.as_ref().is_some_and(|p| p.spec == *s) {
            return Ok(());
        }
        let w = s.r.right - s.r.left;
        let vis = match parts.time.take() {
            Some(p) => p.vis,
            None => {
                let v = self.gfx.compositor.CreateSpriteVisual()?;
                host.Children()?.InsertAtTop(&v)?;
                v
            }
        };
        vis.SetSize(Vector2 { X: w, Y: TRACK_H })?;
        let (x, y) = self.placed(s.r.left, s.r.top, w);
        vis.SetOffset(Vector3 { X: x, Y: y, Z: 0.0 })?;
        vis.SetBrush(&self.time_surface(s)?)?;
        parts.time = Some(TimePart { spec: s.clone(), vis });
        Ok(())
    }

    /// Brings a view's parts (`sub`: the list's) to what the last paint
    /// asked for: made, moved, recolored (cross-fading), pressed or
    /// released, or taken away.
    pub(super) fn sync_parts(&self, plan: Vec<Spec>, sub: bool) {
        let mut parts = if sub { self.sub_parts.borrow_mut() } else { self.parts.borrow_mut() };
        let host = self.host(sub);
        let r = (|| -> Result<()> {
            let mut discs = std::mem::take(&mut parts.discs);
            let mut pills = std::mem::take(&mut parts.pills);
            let mut art: Option<(D2D_RECT_F, Option<Arc<Art>>)> = None;
            let mut time = None;
            for spec in plan {
                match spec {
                    Spec::Disc(s) => {
                        let part = match discs.iter().position(|p| p.spec.hit == s.hit) {
                            Some(i) => {
                                let mut p = discs.remove(i);
                                self.update_disc(&mut parts, &mut p, &s)?;
                                p
                            }
                            None => self.make_disc(&mut parts, &s)?,
                        };
                        parts.discs.push(part);
                    }
                    Spec::Pill(s) => {
                        let part = match pills.iter().position(|p| p.spec.hit == s.hit) {
                            Some(i) => {
                                let mut p = pills.remove(i);
                                self.update_pill(&mut p, &s)?;
                                p
                            }
                            None => self.make_pill(&parts, &s)?,
                        };
                        parts.pills.push(part);
                    }
                    Spec::Art(r, a) => art = Some((r, a)),
                    Spec::Time(s) => time = Some(s),
                }
            }
            for p in discs {
                let _ = host.Children().and_then(|c| c.Remove(&p.vis));
            }
            for p in pills {
                let _ = host.Children().and_then(|c| c.Remove(&p.vis));
            }
            match art {
                Some((r, a)) => {
                    let first = parts.art.is_none();
                    let mut p = match parts.art.take() {
                        Some(p) => p,
                        None => self.make_art(&parts)?,
                    };
                    let done = self.update_art(&mut p, r, a.as_ref(), first);
                    parts.art = Some(p);
                    done?;
                }
                None => {
                    if let Some(p) = parts.art.take() {
                        let _ = host.Children().and_then(|c| c.Remove(&p.vis));
                    }
                }
            }
            self.sync_time(&mut parts, time.as_ref())
        })();
        if let Err(e) = r {
            crate::log!("control: parts: {e}");
        }
    }

    /// Updates only the grid's parts given (a slider dragged, the time
    /// ticking), leaving the rest as they are.
    pub(super) fn sync_parts_only(&self, plan: Vec<Spec>) {
        let mut parts = self.parts.borrow_mut();
        let r = (|| -> Result<()> {
            for spec in plan {
                match spec {
                    Spec::Pill(s) => {
                        if let Some(i) = parts.pills.iter().position(|p| p.spec.hit == s.hit) {
                            let mut p = parts.pills.remove(i);
                            let done = self.update_pill(&mut p, &s);
                            parts.pills.insert(i, p);
                            done?;
                        }
                    }
                    Spec::Time(s) => self.sync_time(&mut parts, Some(&s))?,
                    _ => {}
                }
            }
            Ok(())
        })();
        if let Err(e) = r {
            crate::log!("control: parts: {e}");
        }
    }
}

/// Pure: `v` DIP on the nearest whole device pixel at `k` px per DIP.
pub(super) fn snap_dip(v: f32, k: f32) -> f32 {
    if k > 0.0 {
        (v * k).round() / k
    } else {
        v
    }
}
