//! Sharp dock icons: the pixel sizes each icon is drawn at and which one is
//! shown when.
//!
//! The composition brush that shows a picture filters it bilinearly, with
//! no mipmaps: shrunk to well under its size it skips source pixels, so
//! its edges step, and crawl while the size changes (the jagged look the
//! neighbors of the pointer had while magnified, one peak picture shrunk
//! to 0.6 of its size); at any other ratio than 1:1 it softens a little.
//! So each icon is drawn (D2D high-quality cubic, Gfx::surface_fit) at a
//! few sizes (`step_slots`):
//!
//! - at its exact resting size in device px for the dock's monitor, shown
//!   1:1 on whole device pixels while the icon rests;
//! - at steps up to the magnification peak, each at most 1/STEP larger
//!   than the one before: a magnified icon shows the smallest one at least
//!   its size, shrunk to no less than MIN_RATIO of it (`pick`).
//!
//! All of them are drawn for the inset size on a backplate when the icon
//! has one, and with the squircle cut into the picture when it is masked
//! (one bitmap sampled, not a picture and a mask sampled apart and
//! multiplied). CompositionMipmapSurface was not used: its levels halve,
//! and between two levels the brush still shrinks the larger one to as
//! little as half (trilinear mixes that with the blurred smaller one).
//!
//! Memory (GPU, held by the compositor; nothing of it in our working set):
//! sum of side^2 x 4 bytes over the steps. 48 DIP at the default peak 1.4:
//! 100 % 48 / 54 / 61 / 68 px = 53 KiB an icon (two pictures, 48 + 77 px,
//! were 32 KiB at the old peak 1.6), 150 % 72 / 81 / 90 / 101 px = 117 KiB
//! (was 73), 200 % 96 / 108 / 120 / 135 px = 209 KiB (was 129); twenty
//! icons: 1.0 / 2.3 / 4.1 MiB (were 0.6 / 1.4 / 2.5). Plated
//! icons are 0.6 of that, plus the shared squircle masks of every step
//! (one per size, for all icons). Source pixels are not kept: a picture of
//! another size is drawn from the disk cache again.

/// Each side of an icon on a backplate is inset by this share of the slot.
pub(super) const PLATE_INSET: f32 = 0.11;

/// Side of the peak square (device px): the icon at the magnification peak
/// on this monitor. Also the icon's key size on the shelf, so docks at
/// different scales never share pictures (it used to have a 64 px floor,
/// under which docks at 100 % and 125 % shared one key).
pub(super) fn big_px(icon: f32, max_scale: f32, scale: f32) -> i32 {
    ((icon * max_scale * scale).ceil() as i32).max(1)
}

/// How an icon is dressed (items::dress_icon).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum Dress {
    /// Full-bleed square art: cut to the squircle.
    Masked,
    /// Shaped art: inset on a squircle backplate.
    Plated,
    /// As it is.
    Plain,
}

/// `squircle`: the look asks for squircles (and the mask could be drawn);
/// `native`: Windows' own apps, the Recycle Bin and stacks keep their own
/// shape (such as file-manager and recycle-bin artwork); `coverage`: icons::analyze.
pub(super) fn dress(squircle: bool, native: bool, coverage: f32) -> Dress {
    if !squircle || native {
        Dress::Plain
    } else if coverage >= 0.9 {
        Dress::Masked
    } else {
        Dress::Plated
    }
}

/// Side of a resting slot (device px): the backplate, or a full icon.
pub(super) fn slot_px(icon: f32, scale: f32) -> u32 {
    (icon * scale).round().max(1.0) as u32
}

/// An icon picture's side for a square of `px`: all of it, or the inset
/// icon on a backplate.
pub(super) fn on_plate(px: u32, plated: bool) -> u32 {
    if plated {
        (px as f32 * (1.0 - 2.0 * PLATE_INSET)).round().max(1.0) as u32
    } else {
        px.max(1)
    }
}

/// Side of the resting icon picture (device px; the dock draws it with
/// on_plate for each slot a dock uses).
#[cfg(test)]
pub(super) fn rest_px(icon: f32, scale: f32, plated: bool) -> u32 {
    on_plate(slot_px(icon, scale), plated)
}

/// Side of the peak icon picture (device px; drawn with on_plate).
#[cfg(test)]
pub(super) fn peak_px(icon: f32, max_scale: f32, scale: f32, plated: bool) -> u32 {
    on_plate(big_px(icon, max_scale, scale).max(1) as u32, plated)
}

/// Least share of its own size a picture is shown at while an icon is
/// magnified (bilinear filtering barely skips a pixel there).
#[cfg(test)]
pub(super) const MIN_RATIO: f32 = 0.85;

/// Largest ratio between two neighboring steps (below MIN_RATIO: room for
/// the rounding to whole pixels, and for the backplate inset's).
pub(super) const STEP: f32 = 0.875;

/// Slot sides (device px) an icon is drawn at, ascending: the resting
/// `slot` first and the peak `big` last, evenly spaced in between (each at
/// most 1/STEP larger than the one before). One when they are the same.
pub(super) fn step_slots(slot: u32, big: u32) -> Vec<u32> {
    let (slot, big) = (slot.max(1), big.max(1));
    if big <= slot {
        return vec![big];
    }
    let span = big as f32 / slot as f32;
    let n = (span.ln() / (1.0 / STEP).ln()).ceil().max(1.0) as i32;
    let mut v: Vec<u32> = (0..=n).map(|i| (slot as f32 * span.powf(i as f32 / n as f32)).round() as u32).collect();
    v[0] = slot;
    if let Some(last) = v.last_mut() {
        *last = big;
    }
    v.dedup();
    v
}

/// The picture sides for `step_slots`, on a backplate or not (on_plate;
/// sides that round to the same are drawn once).
pub(super) fn step_sides(slot: u32, big: u32, plated: bool) -> Vec<u32> {
    let mut v: Vec<u32> = step_slots(slot, big).into_iter().map(|s| on_plate(s, plated)).collect();
    v.dedup();
    v
}

/// Every side (device px, ascending, each once) the docks resting at
/// `slots` draw an icon whose peak is `big` at, full-bleed (masked or
/// plain; what the icon thread is told along with the peak: the dock's own
/// tiles are drawn at those of them they look different at).
pub(super) fn ask_sides(slots: &[u32], big: u32) -> Vec<u32> {
    let mut v: Vec<u32> = slots.iter().flat_map(|&s| step_slots(s, big)).collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// Which of the pictures `sides` (ascending; the resting one first) an
/// icon drawn `drawn` px wide shows: the resting one up to a pixel past its
/// size (settling: about 1:1), else the smallest one at least as large
/// (shrunk by at most one step), else the largest.
pub(super) fn pick(drawn: f32, sides: &[u32]) -> usize {
    match sides.first() {
        None => 0,
        Some(&rest) if drawn <= rest as f32 + 1.0 => 0,
        Some(_) => sides.iter().position(|&s| s as f32 >= drawn).unwrap_or(sides.len() - 1),
    }
}

/// A square of `side` device px centered on (cx, cy) DIP, moved onto whole
/// device pixels (the window and the root scale are pixel aligned): its
/// top-left and side, in DIP.
pub(super) fn snap_square(cx: f32, cy: f32, side: u32, scale: f32) -> (f32, f32, f32) {
    let s = side as f32;
    let x = (cx * scale - s / 2.0).round();
    let y = (cy * scale - s / 2.0).round();
    (x / scale, y / scale, s / scale)
}

/// Device px over which an icon settling to its resting size (or growing
/// from it) slides onto its whole-pixel square: no jump on the frame the
/// spring stops.
pub(super) const SNAP_RANGE_PX: f32 = 3.0;

/// How much of the whole-pixel correction an icon `side` device px wide
/// gets when it rests at `rest` px (continuous): all of it at rest, none
/// from SNAP_RANGE_PX away (magnified, appearing, leaving).
pub(super) fn snap_weight(side: f32, rest: f32) -> f32 {
    (1.0 - (side - rest).abs() / SNAP_RANGE_PX).clamp(0.0, 1.0)
}

/// The square an icon is drawn in: `cur` (x, y, side in DIP, continuous)
/// moved by `s` (snap_weight) of the correction that puts its resting
/// square - `rest_side` DIP centered on `rest_c` - onto whole device pixels
/// at `px` (snap_square). The correction is that of the resting square, so
/// it stays put while the icon's own size settles; at `s` = 1 (at rest) it
/// is exactly the snapped square.
pub(super) fn settle_square(cur: (f32, f32, f32), rest_c: (f32, f32), rest_side: f32, px: u32, scale: f32, s: f32) -> (f32, f32, f32) {
    let snapped = snap_square(rest_c.0, rest_c.1, px, scale);
    if s >= 1.0 {
        return snapped;
    }
    if s <= 0.0 {
        return cur;
    }
    let (sx, sy, sd) = snapped;
    let half = rest_side / 2.0;
    let (dx, dy, dd) = (sx - (rest_c.0 - half), sy - (rest_c.1 - half), sd - rest_side);
    (cur.0 + s * dx, cur.1 + s * dy, cur.2 + s * dd)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCALES: [f32; 7] = [1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];
    const ICONS: [f32; 5] = [24.0, 32.0, 48.0, 64.0, 128.0];
    const PEAKS: [f32; 5] = [1.0, 1.3, 1.6, 2.0, 3.0];

    #[test]
    fn dressing() {
        assert_eq!(dress(true, false, 1.0), Dress::Masked);
        assert_eq!(dress(true, false, 0.5), Dress::Plated);
        assert_eq!(dress(true, true, 0.5), Dress::Plain);
        assert_eq!(dress(false, false, 0.5), Dress::Plain);
    }

    #[test]
    fn the_monitors_from_the_report() {
        // 48 DIP icons, default peak 1.6: (rest, rest on a plate, peak key
        // size, peak on a plate, asked of the shell).
        let sizes = |scale: f32| {
            let big = big_px(48.0, 1.6, scale);
            (rest_px(48.0, scale, false), rest_px(48.0, scale, true), big, peak_px(48.0, 1.6, scale, true))
        };
        assert_eq!(sizes(1.0), (48, 37, 77, 60)); // qHD at 100 %
        assert_eq!(sizes(1.25), (60, 47, 96, 75)); // qHD at 125 %
        assert_eq!(sizes(1.5), (72, 56, 116, 90)); // 4K at 150 %
        assert_eq!(sizes(2.0), (96, 75, 154, 120)); // the 2560x1600 panel
        assert_eq!(sizes(3.0), (144, 112, 231, 180));
        // The steps drawn at the default peak 1.4 (the memory note above).
        assert_eq!(step_slots(slot_px(48.0, 1.0), big_px(48.0, 1.4, 1.0) as u32), [48, 54, 61, 68]);
        assert_eq!(step_slots(slot_px(48.0, 1.5), big_px(48.0, 1.4, 1.5) as u32), [72, 81, 90, 101]);
        assert_eq!(step_slots(slot_px(48.0, 2.0), big_px(48.0, 1.4, 2.0) as u32), [96, 108, 120, 135]);
        assert_eq!(step_slots(48, 48), [48]);
        assert_eq!(step_slots(48, 49), [48, 49]);
    }

    #[test]
    fn at_rest_every_icon_is_drawn_at_its_own_size() {
        for s in SCALES {
            for icon in ICONS {
                for peak in PEAKS {
                    for plated in [false, true] {
                        let rest = rest_px(icon, s, plated);
                        let big = peak_px(icon, peak, s, plated);
                        // The resting picture is what the slot shows (to
                        // within the rounding to whole pixels) ...
                        let want = icon * s * if plated { 1.0 - 2.0 * PLATE_INSET } else { 1.0 };
                        assert!((rest as f32 - want).abs() <= 1.0, "{icon} DIP x {s}: {rest} px for {want}");
                        // ... never larger than the peak one, and shown at rest.
                        assert!(rest <= big, "{icon} x {peak} x {s}");
                        let sides = step_sides(slot_px(icon, s), big_px(icon, peak, s) as u32, plated);
                        assert_eq!(sides[0], rest);
                        assert_eq!(pick(rest as f32, &sides), 0);
                    }
                }
            }
        }
    }

    #[test]
    fn at_every_magnification_a_picture_is_shown_at_085_of_its_size_or_more() {
        for s in SCALES {
            for icon in ICONS {
                for peak in PEAKS.into_iter().chain([1.4, 2.5]) {
                    for plated in [false, true] {
                        let (slot, big) = (slot_px(icon, s), big_px(icon, peak, s) as u32);
                        let sides = step_sides(slot, big, plated);
                        let plates = step_slots(slot, big);
                        let k = if plated { 1.0 - 2.0 * PLATE_INSET } else { 1.0 };
                        let mut m = 1.0f32;
                        while m <= peak + 1e-4 {
                            // The icon picture, and the backplate's mask.
                            for (drawn, sides) in [(icon * m * s * k, &sides), (icon * m * s, &plates)] {
                                let side = sides[pick(drawn, sides)] as f32;
                                let ratio = (drawn / side).min(side / drawn);
                                assert!(ratio >= MIN_RATIO, "{icon} DIP x {m} x {s} (peak {peak}, plated {plated}): {drawn} px from {side} ({sides:?})");
                            }
                            m += 0.005;
                        }
                        // At the peak, the peak picture at about its size.
                        let at_peak = icon * peak * s * k;
                        let i = pick(at_peak, &sides);
                        assert!((at_peak - sides[i] as f32).abs() <= 1.5, "{icon} x {peak} x {s}: {at_peak} of {sides:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn steps_are_few_and_even() {
        // Without magnification one picture (the peak is the slot, or a
        // pixel over: then two, the resting one 1:1).
        for s in SCALES {
            for icon in ICONS {
                assert!(step_slots(slot_px(icon, s), big_px(icon, 1.0, s) as u32).len() <= 2, "{icon} x {s}");
            }
        }
        // 1.4: four pictures at 48 DIP and 100 %; 1.6: five; 2.0: seven.
        assert_eq!(step_slots(48, 68).len(), 4);
        assert_eq!(step_slots(48, 77).len(), 5);
        assert_eq!(step_slots(48, 96).len(), 7);
        for (a, b) in [(37u32, 60u32), (48, 77), (96, 192), (19, 22)] {
            let v = step_slots(a, b);
            assert_eq!((v[0], *v.last().unwrap()), (a, b));
            assert!(v.windows(2).all(|w| w[0] < w[1] && w[0] as f32 / w[1] as f32 >= MIN_RATIO), "{v:?}");
        }
    }

    #[test]
    fn docks_at_different_scales_never_share_a_key() {
        for icon in ICONS {
            for peak in PEAKS {
                let keys: Vec<i32> = SCALES.iter().map(|&s| big_px(icon, peak, s)).collect();
                for (i, a) in keys.iter().enumerate() {
                    assert!(!keys[i + 1..].contains(a), "{icon} x {peak}: {keys:?}");
                }
            }
        }
    }

    #[test]
    fn resting_icons_land_on_whole_pixels() {
        for s in SCALES {
            for (cx, cy) in [(10.0f32, 20.0f32), (33.3, 41.7), (100.49, 7.51), (0.2, 0.9)] {
                for side in [37u32, 48, 60, 75, 96] {
                    let (x, y, d) = snap_square(cx, cy, side, s);
                    let near = |v: f32| (v - v.round()).abs() < 1e-3;
                    assert!(near(x * s) && near(y * s), "{cx},{cy} x {s}");
                    assert!((d * s - side as f32).abs() < 1e-3);
                    // Still centered to within half a pixel.
                    assert!(((x + d / 2.0) - cx).abs() * s <= 0.5 + 1e-3);
                    assert!(((y + d / 2.0) - cy).abs() * s <= 0.5 + 1e-3);
                }
            }
        }
    }

    /// The plate and icon squares one frame draws (as layout does): the
    /// continuous ones, bottom anchored at `base`, left edge at `left`.
    fn frame(icon: f32, mag: f32, s: f32, plated: bool, left: f32, base: f32) -> ((f32, f32, f32), (f32, f32, f32), f32) {
        let w = icon * mag;
        let top = base - w;
        let rest_c = (left + icon / 2.0, base - icon / 2.0);
        let wt = snap_weight(w * s, icon * s);
        let plate = settle_square((left, top, w), rest_c, icon, slot_px(icon, s), s, wt);
        let k = if plated { PLATE_INSET } else { 0.0 };
        let (inset, rest_side) = (w * k, icon * (1.0 - 2.0 * k));
        let pic = settle_square((left + inset, top + inset, w - inset * 2.0), rest_c, rest_side, rest_px(icon, s, plated), s, wt);
        (plate, pic, wt)
    }

    fn edges(r: (f32, f32, f32), s: f32) -> [f32; 4] {
        [r.0 * s, r.1 * s, (r.0 + r.2) * s, (r.1 + r.2) * s]
    }

    #[test]
    fn settling_never_jumps() {
        use super::super::layout::{spring_settled, spring_step};
        for s in SCALES {
            for icon in [32.0f32, 48.0, 64.0] {
                for plated in [false, true] {
                    for (left, base) in [(10.0f32, 80.0f32), (33.3, 71.7), (100.49, 64.51)] {
                        for dt in [1.0 / 60.0, 1.0 / 144.0] {
                            for (from, to) in [(1.6f32, 1.0f32), (1.0, 1.6), (1.0, 1.05), (1.05, 1.0)] {
                                let rest = rest_px(icon, s, plated);
                                let sides = step_sides(slot_px(icon, s), big_px(icon, 1.6, s) as u32, plated);
                                let (mut m, mut v) = (from, 0.0f32);
                                let mut prev = frame(icon, m, s, plated, left, base);
                                let mut prev_cont = icon * m;
                                let mut prev_big = pick(prev.1 .2 * s, &sides) > 0;
                                for _ in 0..200 {
                                    (m, v) = spring_step(m, v, to, dt);
                                    if spring_settled((m - to) * icon, v * icon) {
                                        (m, v) = (to, 0.0);
                                    }
                                    let now = frame(icon, m, s, plated, left, base);
                                    // Each edge moves as the continuous square does, give or
                                    // take half a device pixel.
                                    let w = icon * m;
                                    let cont = |w: f32| {
                                        let k = if plated { PLATE_INSET } else { 0.0 };
                                        ((left, base - w, w), (left + w * k, base - w + w * k, w * (1.0 - 2.0 * k)))
                                    };
                                    let (c0, c1) = (cont(prev_cont), cont(w));
                                    for (a, b, ca, cb) in [(prev.0, now.0, c0.0, c1.0), (prev.1, now.1, c0.1, c1.1)] {
                                        let (ea, eb, fa, fb) = (edges(a, s), edges(b, s), edges(ca, s), edges(cb, s));
                                        for i in 0..4 {
                                            // Near rest (the icon growing or shrinking under half
                                            // the snap range a frame) half a pixel; anywhere, never
                                            // more than the correction itself (rounding: half a
                                            // pixel, plus half the rounding of the side).
                                            let jump = ((eb[i] - ea[i]) - (fb[i] - fa[i])).abs();
                                            let grew = (w - prev_cont).abs() * s;
                                            let most = if grew <= SNAP_RANGE_PX / 2.0 { 0.5 } else { 0.8 };
                                            assert!(jump <= most + 1e-3, "{icon} x {s} plated {plated} {from}->{to} at m {m}: edge {i} jumps {jump} (size {grew} px)");
                                        }
                                    }
                                    // The picture changes where both show it about the
                                    // same: the resting one within a pixel of its own size.
                                    let big = pick(now.1 .2 * s, &sides) > 0;
                                    if big != prev_big {
                                        let at = if big { prev.1 .2 } else { now.1 .2 } * s;
                                        assert!((at - rest as f32).abs() <= 1.0 + 1e-3, "{icon} x {s} plated {plated}: swap at {at} px for {rest}");
                                    }
                                    (prev, prev_cont, prev_big) = (now, w, big);
                                    if m == to && v == 0.0 {
                                        break;
                                    }
                                }
                                // At rest: exactly on whole pixels at the resting size.
                                if to == 1.0 {
                                    assert_eq!(prev.2, 1.0);
                                    let near = |v: f32| (v - v.round()).abs() < 1e-3;
                                    for e in edges(prev.0, s).into_iter().chain(edges(prev.1, s)) {
                                        assert!(near(e), "{icon} x {s}: {e}");
                                    }
                                    assert!((prev.1 .2 * s - rest as f32).abs() < 1e-3);
                                    assert!(!prev_big);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
