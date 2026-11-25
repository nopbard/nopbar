//! Magnification, animation tick, layout and the hover label.

use super::*;

/// Magnification follows the pointer on FOLLOW (crate::motion: m = 1,
/// k = 1600, c = 80; critical damping, natural frequency 40 rad/s: 95 % of
/// a step in ~119 ms, settled well inside 250 ms, never overshooting).
pub(super) const FOLLOW: crate::motion::Spring = crate::motion::SPRING_FOLLOW;
/// Icons coming, going and moving to a new slot glide (7.4: what stays
/// moves over on GLIDE; what comes grows in on it).
pub(super) const GLIDE: crate::motion::Spring = crate::motion::SPRING_GLIDE;
/// Snap to the target once the error is below this many DIPs ...
const SPRING_SNAP_DIP: f32 = 0.05;
/// ... and the motion slower than this (DIP/s).
const SPRING_SNAP_VEL: f32 = 5.0;
/// The "Spring" effect off (the lighter presets): magnification follows
/// on a plain exponential follower at this rate (1 / s), no spring.
const PLAIN_FOLLOW: f32 = 13.2;

/// One step of the magnification spring towards `target`, solved exactly
/// (crate::motion::Spring::at: stable for any `dt`). Returns the new
/// (position, velocity).
pub(super) fn spring_step(x: f32, v: f32, target: f32, dt: f32) -> (f32, f32) {
    if x == target && v == 0.0 {
        return (x, v);
    }
    FOLLOW.at(x, v, target, dt.max(0.0))
}

/// One step of GLIDE from `x` (moving at `v`) towards `target`; at rest
/// on it once within `eps` and slow: (position, velocity, still moving).
pub(super) fn glide_step(x: f32, v: f32, target: f32, dt: f32, eps: f32) -> (f32, f32, bool) {
    if x == target && v == 0.0 {
        return (x, 0.0, false);
    }
    let (nx, nv) = GLIDE.at(x, v, target, dt.max(0.0));
    if (nx - target).abs() < eps && nv.abs() < eps * GLIDE.w0() {
        (target, 0.0, false)
    } else {
        (nx, nv, true)
    }
}

/// Close enough to stop the ticker (error and speed in DIPs).
pub(super) fn spring_settled(err_dip: f32, vel_dip: f32) -> bool {
    err_dip.abs() < SPRING_SNAP_DIP && vel_dip.abs() < SPRING_SNAP_VEL
}

/// Presence snaps to its target this close (a 0.3 % sliver of a slot, too
/// small to see): an icon coming or going is over in ~0.45 s, not ~0.6 s
/// of ticks moving it by nothing (polish audit F23).
pub(super) const PRES_SNAP: f32 = 0.003;

impl Dock {
    // ---- animation & layout ------------------------------------------------

    /// Width of one icon slot (a drop gap is this wide when fully open).
    pub(super) fn slot_w(&self) -> f32 {
        self.look.icon + self.look.gap
    }

    pub(super) fn base_center(&self, idx: usize) -> f32 {
        let slot = self.slot_w();
        let total: f32 =
            self.items.iter().map(|it| (it.base_w(self.look.icon) + self.look.gap) * it.pres + it.gap * slot).sum::<f32>() - self.look.gap;
        let mut x = (self.along - total) / 2.0;
        for (i, it) in self.items.iter().enumerate() {
            x += it.gap * slot;
            let w = it.base_w(self.look.icon) * it.pres;
            if i == idx {
                return x + w / 2.0;
            }
            x += w + self.look.gap * it.pres;
        }
        x
    }

    /// Something this dock opened is up (6.2 "open"): its menu (any menu
    /// of ours: it is modal), or its window cards or stack while the
    /// pointer is off the dock, on them.
    pub(super) fn holds_open(&self) -> bool {
        held_open(
            crate::popup::is_tracking(),
            self.mouse.is_some(),
            (views::owns_preview(self.hwnd) && preview::is_open()) || (views::owns_stack(self.hwnd) && stack::is_open()),
        )
    }

    pub(super) fn update_targets(&mut self) {
        // Open (6.2): the magnification stays where it was while what the
        // dock opened is up; it lets go when that closes (release_open).
        if !self.dragging() && self.holds_open() {
            if !std::mem::replace(&mut self.held, true) {
                unsafe {
                    SetTimer(Some(self.hwnd), HOLD_TIMER, HOLD_CHECK_MS, None);
                }
            }
            self.kick();
            return;
        }
        if std::mem::take(&mut self.held) {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), HOLD_TIMER);
            }
        }
        // An icon dragged off the dock: the dock rests.
        let mx = if self.drag_away() { None } else { self.mouse.map(|m| m.0) };
        for i in 0..self.items.len() {
            let t = match (mx, self.items[i].kind) {
                (Some(mx), k) if !k.is_line() => magnified((mx - self.base_center(i)).abs() / self.look.range, self.look.max_scale),
                _ => 1.0,
            };
            self.items[i].mag_t = t;
        }
        self.kick();
    }

    /// What held the magnification may have closed: if so it follows the
    /// pointer again (or rests, the pointer gone). HOLD_TIMER and the end
    /// of a menu ask.
    pub(super) fn release_open(&mut self) {
        if self.held && !self.holds_open() {
            self.update_targets();
            self.layout_label();
        }
    }

    pub(super) fn kick(&mut self) {
        if !self.ticker.animating.swap(true, Ordering::SeqCst) {
            self.last_tick = Instant::now();
            self.ticker.unpark();
        }
    }

    pub(super) fn tick(&mut self) {
        let now = Instant::now();
        let fx = crate::effects::current();
        // At the chosen animation speed.
        let dt = (now - self.last_tick).as_secs_f32().min(0.05) * fx.speed;
        self.last_tick = now;
        // Reduced motion (12.2): nothing springs or slides; magnification
        // stays, following the pointer at once (it is handling, not an
        // animation), and icons come, go and move at once.
        let moves = glides();
        let kp = 1.0 - (-dt * PLAIN_FOLLOW).exp();
        // Magnification is a scale; the spring settles on DIPs of icon size.
        let unit = self.look.icon;
        let mut busy = false;
        for it in &mut self.items {
            // Spring off: a plain follower (no overshoot).
            if moves && !fx.items.spring {
                let d = it.mag_t - it.mag;
                if (d * unit).abs() > 0.05 {
                    it.mag += d * kp;
                    it.mag_v = 0.0;
                    busy = true;
                } else {
                    it.mag = it.mag_t;
                    it.mag_v = 0.0;
                }
            } else if moves {
                // Magnification: FOLLOW, critically damped.
                let (x, v) = spring_step(it.mag, it.mag_v, it.mag_t, dt);
                if spring_settled((x - it.mag_t) * unit, v * unit) {
                    it.mag = it.mag_t;
                    it.mag_v = 0.0;
                } else {
                    it.mag = x;
                    it.mag_v = v;
                    busy = true;
                }
            } else {
                it.mag = it.mag_t;
                it.mag_v = 0.0;
            }
            if !moves {
                (it.pres, it.pres_v) = (it.pres_t, 0.0);
                (it.gap, it.gap_v) = (it.gap_t, 0.0);
                (it.slide, it.slide_v) = (0.0, 0.0);
            } else {
                // Presence (appear / leave), the drop gap opening and
                // closing, the slide to a new slot: GLIDE, each from where
                // it is at the speed it has (7.2 rule 1).
                let (x, v, on) = glide_step(it.pres, it.pres_v, it.pres_t, dt, PRES_SNAP);
                // (Never past empty or whole: GLIDE's last hair of overshoot.)
                (it.pres, it.pres_v) = if (0.0..=1.0).contains(&x) { (x, v) } else { (x.clamp(0.0, 1.0), 0.0) };
                busy |= on;
                let (x, v, on) = glide_step(it.gap, it.gap_v, it.gap_t, dt, 0.0005);
                (it.gap, it.gap_v) = (x.max(0.0), v);
                busy |= on;
                let (x, v, on) = glide_step(it.slide, it.slide_v, 0.0, dt, 0.05);
                (it.slide, it.slide_v) = (x, v);
                busy |= on;
            }
            if let Some(start) = it.bounce {
                // (Policy changed under way: Windows' animation effects or
                // the bounce effect off stop it; items::carried_bounce.)
                if !(motion() && fx.items.bounce) || (now - start).as_secs_f32() > crate::effects::secs(BOUNCE_SECS) {
                    it.bounce = None;
                } else {
                    busy = true;
                }
            }
        }
        // A pressed icon let go of: its scale eases back.
        if let Some((_, t)) = self.press_back {
            if (now - t).as_secs_f32() * 1000.0 < press_back().0 {
                busy = true;
            } else {
                self.press_back = None;
            }
        }
        // drop unpinned items that finished fading out
        let layer = self.layer.clone();
        self.items.retain_mut(|it| {
            let dead = it.kind == Kind::App && !it.pinned && it.pres_t == 0.0 && it.pres < 0.01;
            if dead {
                if let Ok(ch) = layer.Children() {
                    let _ = ch.Remove(&it.icon);
                    let _ = ch.Remove(&it.plate);
                    let _ = ch.Remove(&it.tile);
                    let _ = ch.Remove(&it.dot);
                    it.marks.remove_from(&ch);
                }
            }
            !dead
        });
        if self.mouse.is_some() {
            self.update_targets_quiet();
        }
        self.layout();
        if !busy {
            self.ticker.animating.store(false, Ordering::SeqCst);
            // Safety net for a lost WM_MOUSELEAVE (seen once in testing): when
            // things settle with "mouse inside" but the cursor is elsewhere,
            // treat it as a leave. One check per settle, not a poll.
            if self.mouse.is_some() && !self.cursor_over_window() {
                crate::log!("missed WM_MOUSELEAVE recovered");
                self.on_mouse_leave();
                return;
            }
            // Settled: shrink the hit region back to the bar if the mouse left.
            self.apply_region();
        }
    }

    pub(super) fn cursor_over_window(&self) -> bool {
        let mut pt = POINT::default();
        unsafe {
            if GetCursorPos(&mut pt).is_err() {
                return true;
            }
            WindowFromPoint(pt) == self.hwnd
        }
    }

    /// Like update_targets but without re-kicking (used inside tick while
    /// presence animates, so magnification follows the moving layout).
    pub(super) fn update_targets_quiet(&mut self) {
        let was = self.ticker.is_animating();
        self.update_targets();
        self.ticker.animating.store(was, Ordering::SeqCst);
    }

    /// Lays everything out in the canonical frame (u along the dock, v
    /// across with the screen edge at v = depth) and maps it onto the window
    /// for the dock's edge.
    /// Shows the separator when running apps or the Recycle Bin follow
    /// the pins, unless a divider ends the pins: that one stands for it
    /// (one line, one slot; dividers::system_sep_shown).
    pub(super) fn place_sep(&mut self) {
        let facts: Vec<(Kind, bool, bool)> = self.items.iter().map(|it| (it.kind, it.pinned, it.pres_t > 0.0)).collect();
        let want = if super::dividers::system_sep_shown(&facts) { 1.0 } else { 0.0 };
        let si = self.sep_index();
        if let Some(sep) = self.items.get_mut(si) {
            if sep.pres_t != want {
                sep.pres_t = want;
                if !glides() {
                    sep.pres = want;
                }
                self.kick();
            }
        }
    }

    pub(super) fn layout(&mut self) {
        // (Items move while dragging: a divider dragged to the end, or
        // away from it, takes the separator's place or gives it back.)
        self.place_sep();
        let widths: Vec<f32> = self
            .items
            .iter()
            .map(|it| it.kind.drawn_w(self.look.icon, it.mag, it.pres))
            .collect();
        let slot = self.slot_w();
        let total: f32 = self.items.iter().zip(&widths).map(|(it, w)| w + self.look.gap * it.pres + it.gap * slot).sum::<f32>() - self.look.gap;
        let f = self.frame;
        self.bar_rect = bar_box(total, self.look.icon, self.along, f.depth);
        let Rect { x: bar_u, y: bar_v, h: bar_th, .. } = self.bar_rect;

        let bw = f.to_win(self.bar_rect);
        let size = Vector2 { X: bw.w, Y: bw.h };
        let _ = self.bar.SetOffset(Vector3 { X: bw.x, Y: bw.y, Z: 0.0 });
        let _ = self.bar.SetSize(size);
        for v in &self.bar_fill {
            let _ = v.SetSize(size);
        }
        // The wallpaper layer covers the whole monitor, shifted so the part
        // behind the bar lines up with the real desktop.
        let m = self.monitor;
        let _ = self.mica.SetOffset(Vector3 {
            X: (m.left - self.origin.x) as f32 / self.scale - bw.x,
            Y: (m.top - self.origin.y) as f32 / self.scale - bw.y,
            Z: 0.0,
        });
        let _ = self.mica.SetSize(Vector2 {
            X: (m.right - m.left) as f32 / self.scale,
            Y: (m.bottom - m.top) as f32 / self.scale,
        });
        let _ = self.bar_geom.SetSize(size);
        let _ = self.border_geom.SetSize(Vector2 { X: bw.w - 1.0, Y: bw.h - 1.0 });
        // The rim: a stroke one device pixel thick (Flat v2: 1 DIP, its
        // edge light: Theme::rim_width), half of it in from the bar's edge
        // (design system v2.1, 2.2; spike S7).
        let px = self.theme.rim_width(crate::theme::Material::Dock, self.scale);
        let rim_r = (self.look.radius - px / 2.0).max(0.0);
        let _ = self.bar_geom.SetCornerRadius(Vector2 { X: self.look.radius, Y: self.look.radius });
        let _ = self.border_geom.SetCornerRadius(Vector2 { X: (self.look.radius - 0.5).max(0.0), Y: (self.look.radius - 0.5).max(0.0) });
        let _ = self.highlight_geom.SetSize(Vector2 { X: (bw.w - px).max(0.0), Y: (bw.h - px).max(0.0) });
        let _ = self.highlight_geom.SetOffset(Vector2 { X: px / 2.0, Y: px / 2.0 });
        let _ = self.highlight_geom.SetCornerRadius(Vector2 { X: rim_r, Y: rim_r });
        let _ = self.highlight_shape.SetStrokeThickness(px);
        // The shadow's window follows the bar (shadow.rs).
        self.shadow_spec.thick = bar_th;
        self.shadow_spec.radius = self.look.radius;
        unsafe { self.shadow.update(&self.gfx, self.hwnd, &self.shadow_spec, (bw.x, bw.y, bw.w, bw.h), f.vertical(), self.scale) };
        if self.hidden {
            // Hidden: the bar may be thicker or the scale new; the shadow's
            // slide follows (set_hidden's distance, worked out again).
            let (x, y) = f.away(f.depth - self.bar_rect.y + 4.0);
            self.shadow.slide_to(x, y, self.scale);
        }

        let place = |v: &SpriteVisual, r: Rect| {
            let w = f.to_win(r);
            let _ = v.SetOffset(Vector3 { X: w.x, Y: w.y, Z: 0.0 });
            w
        };
        // The edge-side line icons rest on.
        let baseline = bar_v + PAD_CROSS + self.look.icon;
        let now = Instant::now();
        let mut u = bar_u + PAD_AXIS;
        // The dragged icon is drawn under the pointer; its slot stays open.
        // Off the dock (remove zone) it dims and keeps clear
        // of the window's far side so "Remove" fits beyond it when it shows.
        let room = if self.drag_view().is_some_and(|d| d.3) { self.remove_label_room() } else { 0.0 };
        let drag = self.drag_view().map(|(p, du, dv, off, armed)| (p.to_string(), du, dv, drag::drag_alpha(off, armed)));
        let mut drag_drawn = None;
        let mut moved = false;
        let mut drawn: Vec<(Vec<HWND>, RECT)> = Vec::new();
        let (scale, slot_px) = (self.scale, sharp::slot_px(self.look.icon, self.scale));
        // The pressed icon dims (not while dragged: the drag has its own);
        // the one let go of comes back as press_back says.
        let pressed = if self.drag.is_some() { None } else { self.pressed.map(|(i, _)| i) };
        let (back, back_how) = (self.press_back, press_back());
        for (idx, (it, w)) in self.items.iter_mut().zip(widths).enumerate() {
            let dragged = matches!(&drag, Some((p, ..)) if it.movable() && *p == it.path);
            u += it.gap * slot;
            // Moved to another slot (reorder, pin, an item before it went
            // away) or just dropped: start from where it was drawn and
            // slide over.
            if it.slot != usize::MAX && (it.slot != idx || it.was_dragged) && !dragged {
                it.slide = if glides() { it.dx - u } else { 0.0 };
                moved |= it.slide != 0.0;
            }
            it.slot = idx;
            it.was_dragged = dragged;
            let ud = match &drag {
                Some((_, du, ..)) if dragged => du - w / 2.0,
                _ => u + it.slide,
            };
            let lift = match it.bounce {
                Some(start) => {
                    // Up fast, down soft: sin over half a turn, eased.
                    let t = ((now - start).as_secs_f32() / crate::effects::secs(BOUNCE_SECS)).clamp(0.0, 1.0);
                    let e = 1.0 - (1.0 - t) * (1.0 - t);
                    (e * PI).sin() * self.look.icon * 0.35
                }
                None => 0.0,
            };
            if it.kind == Kind::Sep {
                let h = self.look.icon * 0.5;
                // One device pixel on a whole pixel (audit #13), not 1 DIP
                // smeared over 1.25 or 1.5 px.
                let (x, lw) = crate::popup::hairline(u + w / 2.0 - 0.5 / self.scale, self.scale);
                let r = place(&it.icon, Rect { x, y: baseline - self.look.icon / 2.0 - h / 2.0, w: lw, h });
                let _ = it.icon.SetSize(Vector2 { X: r.w, Y: r.h });
                let _ = it.icon.SetOpacity(it.pres);
            } else if it.kind == Kind::Divider {
                // The separator's line, in its own slot: it slides when
                // the order changes, and dragged it follows the pointer
                // (across too, off the dock to be removed), dimmed as a
                // dragged icon is.
                let (line_top, h) = super::dividers::line_span(baseline, self.look.icon);
                let rest = line_top + h / 2.0;
                let mid = match &drag {
                    Some((_, _, dv, _)) if dragged => dv.clamp((room + h / 2.0).min(rest), rest),
                    _ => rest,
                };
                let alpha = match &drag {
                    Some((.., a)) if dragged => it.pres * a,
                    _ => it.pres,
                };
                if dragged {
                    drag_drawn = Some((ud + w / 2.0, mid - h / 2.0));
                }
                let (x, lw) = crate::popup::hairline(ud + w / 2.0 - 0.5 / self.scale, self.scale);
                let r = place(&it.icon, Rect { x, y: mid - h / 2.0, w: lw, h });
                let _ = it.icon.SetSize(Vector2 { X: r.w, Y: r.h });
                let _ = it.icon.SetOpacity(alpha);
            } else {
                // Appearing / leaving: fade with presence (size already
                // follows it, so neighbours make or close the gap).
                let alpha = it.pres.clamp(0.0, 1.0);
                let alpha = match &drag {
                    Some((.., a)) if dragged => alpha * a,
                    _ => alpha,
                };
                // (The indicator keeps its own fade: it has an implicit
                // opacity animation and follows the window list.)
                // Not responding: the icon dims (its badge stays bright).
                let dim = if it.hung { hung::HUNG_DIM } else { 1.0 };
                let since = back.filter(|(k, _)| *k == idx).map(|(_, t)| (now - t).as_secs_f32() * 1000.0);
                // (High contrast: no dim, an outline instead: ring.rs.)
                let press = press_scale(pressed == Some(idx), since, back_how);
                let _ = it.icon.SetOpacity(alpha * dim);
                let _ = it.plate.SetOpacity(if it.plated { alpha * dim } else { 0.0 });
                let rest = icon_top(self.bar_rect, self.look.icon, w) - lift;
                let top = match &drag {
                    // Follows the pointer across the dock too (never past
                    // its resting place towards the edge).
                    Some((_, _, dv, _)) if dragged => (dv - w / 2.0).clamp(room.min(rest), rest),
                    _ => rest,
                };
                if dragged {
                    drag_drawn = Some((ud + w / 2.0, top));
                }
                // At rest the resting pictures, 1:1 on whole device pixels;
                // magnified, the step at or just above the size drawn
                // (sharp.rs). Settling, the squares
                // slide onto those pixels over the last SNAP_RANGE_PX of
                // size (no jump on the frame the spring stops). Only with
                // pictures drawn for this scale and dress (a DPI change
                // redraws them; a failed resting picture is retried).
                let icon_dip = self.look.icon;
                let plated = it.plated;
                let fits = it.pics.as_ref().is_some_and(|p| p.rest_px() == sharp::on_plate(slot_px, plated));
                let weight = if fits { sharp::snap_weight(w * scale, icon_dip * scale) } else { 0.0 };
                // The resting square keeps the left and edge-side sides.
                let rest_c = (ud + icon_dip / 2.0, top + w - icon_dip / 2.0);
                // (In window coordinates: those are the ones on the pixel grid.)
                let settle = |r: Rect, rest_side: f32, px: u32| {
                    let (cur, half) = (f.to_win(r), rest_side / 2.0);
                    let rest = f.to_win(Rect { x: rest_c.0 - half, y: rest_c.1 - half, w: rest_side, h: rest_side });
                    let c = (rest.x + rest.w / 2.0, rest.y + rest.h / 2.0);
                    let (x, y, d) = sharp::settle_square((cur.x, cur.y, cur.w), c, rest_side, px, scale, weight);
                    Rect { x, y, w: d, h: d }
                };
                let put = |v: &SpriteVisual, r: Rect| {
                    let _ = v.SetOffset(Vector3 { X: r.x, Y: r.y, Z: 0.0 });
                    r
                };
                let inset = if plated { w * sharp::PLATE_INSET } else { 0.0 };
                let icon_rect = settle(
                    Rect { x: ud + inset, y: top + inset, w: w - inset * 2.0, h: w - inset * 2.0 },
                    icon_dip * if plated { 1.0 - 2.0 * sharp::PLATE_INSET } else { 1.0 },
                    sharp::on_plate(slot_px, plated),
                );
                let plate_rect = settle(Rect { x: ud, y: top, w, h: w }, icon_dip, slot_px);
                let tile_rect = f.to_win(Rect { x: ud - 3.0, y: top - 3.0, w: w + 6.0, h: w + 6.0 });
                put(&it.tile, tile_rect);
                let tile_size = Vector2 { X: tile_rect.w, Y: tile_rect.h };
                let _ = it.tile.SetSize(tile_size);
                let _ = it.tile_geom.SetSize(tile_size);
                let hover = self.hover_item == Some(idx);
                let fill = tile_color(&self.theme, hover, it.active);
                let brush_fill = Rgba { a: 255, ..fill };
                if it.tile_fill != Some(brush_fill) {
                    if let Ok(brush) = self.gfx.compositor.CreateColorBrushWithColor(rgba(brush_fill)) {
                        let _ = it.tile.SetBrush(&brush);
                        it.tile_fill = Some(brush_fill);
                    }
                }
                let _ = it.tile.SetOpacity(if dragged { 0.0 } else { alpha * fill.a as f32 / 255.0 });
                for v in [&it.icon, &it.plate] {
                    let side = if v == &it.icon { icon_rect.w } else { plate_rect.w };
                    let _ = v.SetCenterPoint(Vector3 { X: side / 2.0, Y: side / 2.0, Z: 0.0 });
                    let _ = v.SetScale(Vector3 { X: press, Y: press, Z: 1.0 });
                }
                it.show_pics(icon_rect.w * scale, plate_rect.w * scale);
                let r = put(&it.plate, plate_rect);
                let _ = it.plate.SetSize(Vector2 { X: r.w, Y: r.h });
                it.marks.place(r, alpha, scale, weight);
                if it.kind == Kind::App && !it.windows.is_empty() {
                    let (s, o) = (self.scale, self.origin);
                    drawn.push((
                        it.windows.clone(),
                        RECT {
                            left: o.x + (r.x * s) as i32,
                            top: o.y + (r.y * s) as i32,
                            right: o.x + ((r.x + r.w) * s) as i32,
                            bottom: o.y + ((r.y + r.h) * s) as i32,
                        },
                    ));
                }
                // On a backplate the icon sits inset at 78%.
                let r = put(&it.icon, icon_rect);
                let _ = it.icon.SetSize(Vector2 { X: r.w, Y: r.h });
                // Centered in the edge-side padding, but never past the bar edge.
                let ind = indicator_size(self.look.indicator, it.active, it.windows.len() > 1);
                let dv = indicator_v(self.bar_rect, ind.Y);
                place(&it.dot, Rect { x: ud + w / 2.0 - ind.X / 2.0, y: dv, w: ind.X, h: ind.Y });
            }
            it.x = u;
            it.dx = ud;
            it.w = w;
            u += w + self.look.gap * it.pres;
        }
        if moved {
            self.kick();
        }
        self.note_drag_drawn(drag_drawn);
        self.publish_minrects(drawn);
        self.companion_ground();
        self.place_drop_glow();
        self.place_rings();
        self.place_split_badge();
        self.layout_label();
        self.apply_region();
        self.publish_a11y();
    }
}

/// How a pressed icon's dim comes back: (ms of real time, curve).
/// crate::motion::FADE_IN_MS on the ease-out (6.3: let go, 90 ms) at the
/// chosen speed; with reduced motion REDUCED_MS straight, the speed left
/// out (crate::motion::fade_plan, like every fade).
fn press_back() -> (f32, crate::motion::Curve) {
    let (ms, curve) = crate::motion::fade_now(crate::motion::FADE_IN_MS, crate::motion::EASE_OUT);
    (ms as f32, curve)
}

/// Pure: an icon's magnification with the pointer `d` falloff radii from
/// its resting center (a raised cosine: the peak under the pointer, rest
/// from one radius on).
pub(super) fn magnified(d: f32, max_scale: f32) -> f32 {
    if d < 1.0 {
        1.0 + (max_scale - 1.0) * (PI * d).cos().mul_add(0.5, 0.5)
    } else {
        1.0
    }
}

/// Pure: the bar in the canonical frame for items `total` DIP long in all
/// (their gaps in it), icons `icon` DIP at rest, in a window `along` by
/// `depth`: the same padding on every side of the icons (1.3, 3.3: with
/// the icons' own gap, "edge to card 8, card to card 8"), centered along
/// the dock, EDGE_GAP off the screen's edge.
pub(super) fn bar_box(total: f32, icon: f32, along: f32, depth: f32) -> Rect {
    let (len, th) = (total + PAD_AXIS * 2.0, icon + PAD_CROSS * 2.0);
    Rect { x: (along - len) / 2.0, y: depth - EDGE_GAP - th, w: len, h: th }
}

/// Pure: where a running indicator `h` DIP thick sits across the dock:
/// in the middle of the edge-side padding (a 4 DIP dot: 2 above, 2 below),
/// never past the bar's edge.
pub(super) fn indicator_v(bar: Rect, h: f32) -> f32 {
    (bar.y + bar.h - PAD_CROSS / 2.0 - h / 2.0).min(bar.y + bar.h - h - 1.0)
}

/// Pure: the top (across the dock, canonical v) of an icon drawn `w` DIP
/// on bar `bar` with icons `icon` DIP at rest: every icon stands on one
/// line, the edge-side padding under it, and grows away from the edge.
pub(super) fn icon_top(bar: Rect, icon: f32, w: f32) -> f32 {
    bar.y + PAD_CROSS + icon - w
}

/// Pure: the hit region stays grown (the window's depth over the bar's
/// span) while the pointer is `on_dock`, anything is `animating`, what the
/// dock opened holds the magnification (`held`, 6.2 open: the pointer is
/// on its cards or its stack, or a menu is up) or an icon is still drawn
/// bigger than at rest (`magnified`). Back to the bar only once all are
/// over: the panel closed and the icons shrunk back.
pub(super) fn region_grown(on_dock: bool, animating: bool, held: bool, magnified: bool) -> bool {
    on_dock || animating || held || magnified
}

/// Pure: the live part of a window `along` by `depth` (canonical frame)
/// over bar `bar`: all of it while `dragging` (a dragged icon goes
/// anywhere), the window's depth over the bar's span (and a little past
/// its ends) while `grown`, else the bar.
pub(super) fn region_rect(bar: Rect, along: f32, depth: f32, dragging: bool, grown: bool) -> Rect {
    if dragging {
        Rect { x: 0.0, y: 0.0, w: along, h: depth }
    } else if grown {
        Rect { x: bar.x - 4.0, y: 0.0, w: bar.w + 8.0, h: depth }
    } else {
        bar
    }
}

/// Pure: the dock holds its magnification (6.2 "open"): a menu is up
/// (`menu`; modal, wherever the pointer is), or its cards or stack are
/// (`panel`) and the pointer is not on the dock (`on_dock`: there it
/// still picks the next icon, and the cards follow it).
pub(super) fn held_open(menu: bool, on_dock: bool, panel: bool) -> bool {
    menu || (panel && !on_dock)
}

/// Pure: the pressed scale, independent of presence and hung opacity:
/// 0.90 while `pressed`; let go `since` ms ago, back
/// to 1 over `back` (ms of real time, curve: press_back); else 1.
pub(super) fn press_scale(pressed: bool, since: Option<f32>, back: (f32, crate::motion::Curve)) -> f32 {
    let (back_ms, curve) = back;
    let low = 0.90;
    if pressed {
        return low;
    }
    match since {
        Some(t) if back_ms > 0.0 && t < back_ms => low + (1.0 - low) * crate::motion::bezier(curve, t / back_ms),
        _ => 1.0,
    }
}

/// Neutral tile behind the icon: hover stronger than persistent foreground.
pub(super) fn tile_color(theme: &Theme, hover: bool, active: bool) -> Rgba {
    if let Some(hc) = theme.hc { return Rgba { a: if hover || active { 255 } else { 0 }, ..hc.highlight }; }
    let c = if theme.is_light { 0 } else { 255 };
    Rgba { r: c, g: c, b: c, a: if hover { 26 } else if active { 15 } else { 0 } }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The press coming back as designed: 90 ms on the ease-out.
    const E90: (f32, crate::motion::Curve) = (90.0, crate::motion::EASE_OUT);

    #[test]
    fn a_pressed_icon_shrinks_and_eases_back() {
        let low = 0.90;
        // Pressed: the scale stays down regardless of layout.
        assert_eq!(press_scale(true, None, E90), low);
        assert_eq!(press_scale(true, Some(10.0), E90), low);
        // Let go: back over 90 ms, never past full.
        let mid = press_scale(false, Some(30.0), E90);
        assert!(mid > low && mid < 1.0, "{mid}");
        assert!(press_scale(false, Some(60.0), E90) >= mid);
        assert_eq!(press_scale(false, Some(90.0), E90), 1.0);
        assert_eq!(press_scale(false, None, E90), 1.0);
        // Without animations: at once.
        assert_eq!(press_scale(false, Some(1.0), (0.0, crate::motion::EASE_OUT)), 1.0);

    }

    /// Steps at 60 Hz from `from` to `to` (scale units, 48 DIP icons) and
    /// returns the settle time in seconds, checking it never overshoots.
    fn settle(from: f32, to: f32, dt: f32) -> f32 {
        let (mut x, mut v, mut t) = (from, 0.0f32, 0.0f32);
        let unit = 48.0;
        while t < 2.0 {
            let (nx, nv) = spring_step(x, v, to, dt);
            // Critically damped: never past the target.
            assert!((nx - to) * (from - to) >= -1e-6, "overshoot at {t}: {nx}");
            x = nx;
            v = nv;
            t += dt;
            if spring_settled((x - to) * unit, v * unit) {
                return t;
            }
        }
        panic!("did not settle");
    }

    #[test]
    fn presence_settles_in_under_half_a_second() {
        // The tick's presence on GLIDE at 60 Hz, 0 -> 1 (an icon coming in).
        let dt = 1.0 / 60.0;
        let (mut pres, mut v, mut t) = (0.0f32, 0.0f32, 0.0f32);
        loop {
            let (x, nv, on) = glide_step(pres, v, 1.0, dt, PRES_SNAP);
            // (GLIDE's overshoot is a hair: under a thousandth of a slot.)
            assert!(x < 1.001, "{x}");
            (pres, v) = (x, nv);
            t += dt;
            if !on {
                break;
            }
        }
        assert_eq!((pres, v), (1.0, 0.0), "at rest on the target");
        assert!(t < 0.5, "{t}");
        // (Still a visible ease, not a jump.)
        assert!(t > 0.25, "{t}");
        // At rest it stays at rest (the ticker stops).
        assert_eq!(glide_step(1.0, 0.0, 1.0, dt, PRES_SNAP), (1.0, 0.0, false));
    }

    #[test]
    fn the_bar_has_the_same_padding_on_every_side() {
        // Twelve 52 DIP icons and a separator, 8 between them all.
        let (icon, gap) = (52.0, crate::config::DOCK_GAP);
        assert_eq!((PAD_AXIS, PAD_CROSS, gap, SEP_W, EDGE_GAP), (8.0, 8.0, 8.0, 8.0, 8.0));
        let total = 12.0 * icon + SEP_W + 12.0 * gap;
        let bar = bar_box(total, icon, 1200.0, 200.0);
        assert_eq!((bar.w, bar.h), (total + 16.0, 68.0));
        // Centered along the dock, 8 off the screen's edge.
        assert_eq!((bar.x, bar.y + bar.h), ((1200.0 - bar.w) / 2.0, 192.0));
        // The first icon is as far from the bar's end as from its sides,
        // so its squircle's corner and the bar's share a center (1.3).
        let (first_u, first_v) = (bar.x + PAD_AXIS, bar.y + PAD_CROSS);
        assert_eq!((first_u - bar.x, first_v - bar.y), (8.0, 8.0));
        for icon in [24.0f32, 36.0, 52.0, 96.0, 128.0] {
            let look = Look::from_cfg(&crate::config::DockCfg { icon_size: icon, ..crate::config::Config::default().dock }, true);
            assert_eq!(look.radius, 8.0, "Windows plate radius at every icon size");
        }
        // Either side of a separator the icons are 24 apart (3.3).
        assert_eq!(gap + SEP_W + gap, 24.0);
        // The running dot: in the middle of the padding under the icons.
        let dot = crate::theme::metrics::DOT;
        let top = indicator_v(bar, dot);
        assert_eq!((top - (bar.y + PAD_CROSS + icon), bar.y + bar.h - (top + dot)), (2.0, 2.0));
    }

    #[test]
    fn magnification_peaks_under_the_pointer_and_rests_a_radius_away() {
        assert_eq!(magnified(0.0, 1.6), 1.6);
        assert!((magnified(0.5, 1.6) - 1.3).abs() < 1e-5);
        assert_eq!(magnified(1.0, 1.6), 1.0);
        assert_eq!(magnified(3.0, 1.6), 1.0);
        assert_eq!(magnified(0.2, 1.0), 1.0, "magnification off");
    }

    #[test]
    fn a_retargeted_glide_keeps_its_speed() {
        // An icon sliding to a new slot is sent on before it arrives: no
        // stop, no jump (7.2 rule 1).
        let dt = 1.0 / 60.0;
        let (mut x, mut v) = (40.0f32, 0.0f32);
        for _ in 0..4 {
            (x, v, _) = glide_step(x, v, 0.0, dt, 0.05);
        }
        assert!(v < 0.0 && x < 40.0);
        // Its displacement grows by another slot: the speed carries over.
        let (nx, nv, on) = glide_step(x + 60.0, v, 0.0, dt, 0.05);
        assert!(on && nv < v && nx < x + 60.0);
    }

    #[test]
    fn the_magnification_holds_while_the_docks_own_surface_is_up() {
        // A menu is modal: held wherever the pointer is.
        assert!(held_open(true, true, false) && held_open(true, false, false));
        // Cards or a stack: held once the pointer is off the dock (on it,
        // the next icon is picked and the cards follow).
        assert!(held_open(false, false, true));
        assert!(!held_open(false, true, true));
        assert!(!held_open(false, false, false) && !held_open(false, true, false));
    }

    #[test]
    fn spring_settles_within_250ms() {
        // Full magnification step up and back down (1.0 <-> 1.6 at 48 DIP).
        assert!(settle(1.0, 1.6, 1.0 / 60.0) < 0.25);
        assert!(settle(1.6, 1.0, 1.0 / 60.0) < 0.25);
        // Largest allowed step (scale 3 on small icons) still settles fast.
        assert!(settle(1.0, 3.0, 1.0 / 144.0) < 0.3);
    }

    #[test]
    fn spring_reaches_95_percent_near_119ms() {
        let (mut x, mut v) = (0.0f32, 0.0f32);
        let dt = 1.0 / 1000.0;
        let mut t = 0.0;
        while x < 0.95 {
            (x, v) = spring_step(x, v, 1.0, dt);
            t += dt;
        }
        assert!((0.11..0.13).contains(&t), "95 % at {t}");
    }

    #[test]
    fn spring_step_is_exact_for_any_dt() {
        // One 50 ms step equals fifty 1 ms steps (no integration error, no
        // blow-up on a long frame).
        let one = spring_step(1.0, 0.0, 1.6, 0.05);
        let mut many = (1.0f32, 0.0f32);
        for _ in 0..50 {
            many = spring_step(many.0, many.1, 1.6, 0.001);
        }
        assert!((one.0 - many.0).abs() < 1e-4 && (one.1 - many.1).abs() < 1e-2, "{one:?} vs {many:?}");
        // At rest on the target it stays there.
        assert_eq!(spring_step(1.2, 0.0, 1.2, 0.016), (1.2, 0.0));
    }

    #[test]
    fn spring_snaps_only_when_slow_and_close() {
        assert!(spring_settled(0.04, 1.0));
        assert!(!spring_settled(0.06, 0.0));
        assert!(!spring_settled(0.01, 20.0), "passing through fast is not settled");
    }

    /// The pressed dim comes back as every fade does (crate::motion::
    /// fade_plan): with reduced motion REDUCED_MS straight at any speed (half
    /// way at half time), with full motion 90 ms at the speed on the
    /// ease-out.
    #[test]
    fn a_press_comes_back_on_the_fade_policy() {
        use crate::motion::{fade_plan, Mode, EASE_OUT, FADE_IN_MS, LINEAR, REDUCED_MS};
        for speed in [0.25f32, 1.0, 4.0] {
            let (ms, curve) = fade_plan(Mode::Reduced, FADE_IN_MS, EASE_OUT, speed);
            assert_eq!((ms, curve), (REDUCED_MS, LINEAR), "speed {speed}");
            let back = (ms as f32, curve);
            let half = press_scale(false, Some(ms as f32 / 2.0), back);
            assert!((half - (0.90 + (1.0 - 0.90) * 0.5)).abs() < 1e-4, "{half}");
            assert_eq!(press_scale(false, Some(ms as f32), back), 1.0);
            let (ms, curve) = fade_plan(Mode::Full, FADE_IN_MS, EASE_OUT, speed);
            assert_eq!((ms, curve), (crate::effects::ms_at(FADE_IN_MS, speed), EASE_OUT), "speed {speed}");
        }
    }

    /// Review P1: the cards (or a stack) hold the magnification while the
    /// pointer is on them; the hit region must hold the magnified icons
    /// too. The tick's path, in order: the pointer leaves for the cards,
    /// the dock holds, the springs settle (the ticker stops) and the
    /// region is worked out; then the cards close, the icons shrink back
    /// and only then the region is the bar again. Every frame, every icon
    /// drawn is inside the region.
    #[test]
    fn an_open_panel_keeps_the_magnified_icons_inside_the_region() {
        let (icon, gap, max) = (52.0f32, crate::config::DOCK_GAP, 1.4f32);
        let range = icon * crate::config::DOCK_RANGE;
        let depth = icon * max + PAD_CROSS + EDGE_GAP + HEADROOM;
        let (along, n) = (1200.0f32, 7usize);
        let center = |i: usize| {
            let total = n as f32 * (icon + gap) - gap;
            (along - total) / 2.0 + i as f32 * (icon + gap) + icon / 2.0
        };
        let targets = |pointer: Option<f32>| -> Vec<f32> {
            (0..n).map(|i| pointer.map_or(1.0, |mx| magnified((mx - center(i)).abs() / range, max))).collect()
        };
        // The bar and the icons as layout() draws them.
        let drawn = |mag: &[f32]| -> (Rect, Vec<Rect>) {
            let widths: Vec<f32> = mag.iter().map(|&m| Kind::App.drawn_w(icon, m, 1.0)).collect();
            let total = widths.iter().map(|w| w + gap).sum::<f32>() - gap;
            let bar = bar_box(total, icon, along, depth);
            let mut u = bar.x + PAD_AXIS;
            let icons = widths
                .iter()
                .map(|&w| {
                    let r = Rect { x: u, y: icon_top(bar, icon, w), w, h: w };
                    u += w + gap;
                    r
                })
                .collect();
            (bar, icons)
        };
        let inside = |r: Rect, o: Rect| r.x >= o.x - 1e-3 && r.y >= o.y - 1e-3 && r.x + r.w <= o.x + o.w + 1e-3 && r.y + r.h <= o.y + o.h + 1e-3;
        // The region apply_region sets for this state.
        let region = |mag: &[f32], on_dock: bool, animating: bool, held: bool| {
            let magnified = mag.iter().any(|&m| m > 1.0);
            region_rect(drawn(mag).0, along, depth, false, region_grown(on_dock, animating, held, magnified))
        };
        // The tick at 60 Hz until the springs settle; `frame` sees every
        // frame (the ticker still running: true) and the settled one.
        let run = |mag: &mut Vec<f32>, t: &[f32], frame: &mut dyn FnMut(&[f32], bool)| {
            let mut vel = vec![0.0f32; n];
            for _ in 0..600 {
                let mut busy = false;
                for i in 0..n {
                    let (x, v) = spring_step(mag[i], vel[i], t[i], 1.0 / 60.0);
                    if spring_settled((x - t[i]) * icon, v * icon) {
                        (mag[i], vel[i]) = (t[i], 0.0);
                    } else {
                        (mag[i], vel[i]) = (x, v);
                        busy = true;
                    }
                }
                frame(mag, busy);
                if !busy {
                    return;
                }
            }
            panic!("did not settle");
        };
        let all_inside = |mag: &[f32], o: Rect| drawn(mag).1.into_iter().all(|r| inside(r, o));

        // The pointer on the fourth icon: magnified.
        let mut mag = vec![1.0f32; n];
        let peak = targets(Some(center(3)));
        run(&mut mag, &peak, &mut |m, busy| assert!(all_inside(m, region(m, true, busy, false))));
        let (bar, icons) = drawn(&mag);
        assert!(icons[3].h > icon * 1.39, "magnified: {}", icons[3].h);
        assert!(!inside(icons[3], bar), "it stands out of the bar (what the bar alone would clip)");

        // It leaves for the cards: held (the targets stay where they were).
        let held = held_open(false, false, true);
        assert!(held);
        run(&mut mag, &peak, &mut |_, _| {});
        // Settled: the ticker stopped, and the region is worked out.
        let r = region(&mag, false, false, held);
        assert!(all_inside(&mag, r), "held: every magnified icon inside {r:?}");
        // Even with the hold already let go of but the icons still drawn
        // big (one frame before the tick), nothing is cut.
        assert!(all_inside(&mag, region(&mag, false, false, false)));

        // The cards close: the dock lets go and the icons shrink back;
        // every frame stays inside, and the settled region is the bar.
        assert!(!held_open(false, false, false));
        run(&mut mag, &targets(None), &mut |m, busy| assert!(all_inside(m, region(m, false, busy, false))));
        assert!(mag.iter().all(|&m| m == 1.0));
        let (bar, _) = drawn(&mag);
        assert_eq!(region(&mag, false, false, false), bar, "back to the bar once all is over");
        assert!(all_inside(&mag, bar));
    }
}
