//! Tray icon normalization: apps pad their icons differently (a 16 px icon
//! may hold a 10 px picture), so icons look different sizes side by side.
//! The icon is rendered at its own size, its opaque bounding box found, and
//! that box scaled (aspect kept) to fill a fixed square with a small inset,
//! centered. Pure functions over premultiplied BGRA.

/// Alpha above which a pixel counts as part of the picture (soft shadows
/// and glows below it do not stretch the box).
pub const ALPHA_THRESHOLD: u8 = 32;
/// Content is never scaled up past this factor over its nominal size (a
/// tiny dot icon must not turn into a blurry blob).
pub const MAX_GROW: f32 = 1.5;

/// Opaque bounding box of a `w` x `h` premultiplied BGRA image:
/// (x0, y0, x1, y1), end exclusive. None if nothing is opaque enough.
pub fn opaque_bbox(px: &[u8], w: u32, h: u32, threshold: u8) -> Option<(u32, u32, u32, u32)> {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for y in 0..h {
        for x in 0..w {
            let a = px.get(((y * w + x) * 4 + 3) as usize).copied().unwrap_or(0);
            if a > threshold {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    (x1 > x0 && y1 > y0).then_some((x0, y0, x1, y1))
}

/// Inset (px) around the content in an `out` px square: 1 DIP, as `out` is
/// the 16 DIP tray size in px.
pub fn inset_px(out: u32) -> f32 {
    (out as f32 / 16.0).round().max(1.0)
}

/// Where the content box (`bw` x `bh` source px, from an image `src` px
/// square) lands in the `out` px square: (x, y, scale). The content fills
/// `out - 2 * inset` on its longer side, centered; the scale is capped at
/// MAX_GROW times the nominal `out / src`. x and y are whole px so content
/// that is not scaled stays sharp.
pub fn fit(bw: f32, bh: f32, src: f32, out: f32, inset: f32) -> (f32, f32, f32) {
    let room = (out - 2.0 * inset).max(1.0);
    let nominal = out / src.max(1.0);
    let scale = (room / bw.max(1.0)).min(room / bh.max(1.0)).min(nominal * MAX_GROW);
    let (w, h) = (bw * scale, bh * scale);
    (((out - w) / 2.0).round(), ((out - h) / 2.0).round(), scale)
}

/// Per output pixel along one axis: the source pixels and their weights.
/// Output pixel d covers source [o + (d - at) / scale, o + (d + 1 - at) /
/// scale). Shrinking averages the covered area; growing is bilinear.
fn weights(out: u32, at: f32, scale: f32, origin: f32, src_len: u32) -> Vec<Vec<(usize, f32)>> {
    (0..out)
        .map(|d| {
            let mut ws = Vec::new();
            if scale <= 1.0 {
                let a = origin + (d as f32 - at) / scale;
                let b = origin + (d as f32 + 1.0 - at) / scale;
                let span = b - a;
                let (lo, hi) = (a.floor().max(0.0) as i64, (b.ceil() as i64).min(src_len as i64));
                for s in lo..hi {
                    let cover = (b.min(s as f32 + 1.0) - a.max(s as f32)).max(0.0);
                    if cover > 0.0 {
                        ws.push((s as usize, cover / span));
                    }
                }
            } else {
                let c = origin + (d as f32 + 0.5 - at) / scale - 0.5;
                let f = c.floor();
                let t = c - f;
                for (s, wgt) in [(f as i64, 1.0 - t), (f as i64 + 1, t)] {
                    if wgt > 0.0 && s >= 0 && s < src_len as i64 {
                        ws.push((s as usize, wgt));
                    }
                }
            }
            ws
        })
        .collect()
}

/// The normalized icon: `src` (an `n` px square) with its opaque box placed
/// per `fit` in an `out` px square. Icons with nothing opaque come back as
/// a plain resample of the whole image.
pub fn normalize(src: &[u8], n: u32, out: u32) -> Vec<u8> {
    let (x0, y0, x1, y1) = opaque_bbox(src, n, n, ALPHA_THRESHOLD).unwrap_or((0, 0, n, n));
    let (bw, bh) = ((x1 - x0) as f32, (y1 - y0) as f32);
    let (ax, ay, scale) = fit(bw, bh, n as f32, out as f32, inset_px(out));
    let wx = weights(out, ax, scale, x0 as f32, n);
    let wy = weights(out, ay, scale, y0 as f32, n);
    let mut px = vec![0u8; (out * out * 4) as usize];
    for (dy, ry) in wy.iter().enumerate() {
        for (dx, rx) in wx.iter().enumerate() {
            let mut acc = [0f32; 4];
            for &(sy, wy) in ry {
                for &(sx, wx) in rx {
                    let i = (sy * n as usize + sx) * 4;
                    let w = wx * wy;
                    for c in 0..4 {
                        acc[c] += src[i + c] as f32 * w;
                    }
                }
            }
            let o = (dy * out as usize + dx) * 4;
            let a = acc[3].round().clamp(0.0, 255.0);
            px[o + 3] = a as u8;
            for c in 0..3 {
                // Premultiplied: color never above alpha.
                px[o + c] = acc[c].round().clamp(0.0, a) as u8;
            }
        }
    }
    px
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(n: u32, x0: u32, y0: u32, x1: u32, y1: u32) -> Vec<u8> {
        let mut v = vec![0u8; (n * n * 4) as usize];
        for y in y0..y1 {
            for x in x0..x1 {
                let i = ((y * n + x) * 4) as usize;
                v[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
        v
    }

    #[test]
    fn bbox_finds_the_picture_and_skips_faint_pixels() {
        let mut img = square(16, 3, 4, 13, 12);
        // A faint shadow pixel outside the picture does not count.
        img[(15 * 16 + 15) * 4 + 3] = 10;
        assert_eq!(opaque_bbox(&img, 16, 16, ALPHA_THRESHOLD), Some((3, 4, 13, 12)));
        assert_eq!(opaque_bbox(&vec![0; 16 * 16 * 4], 16, 16, ALPHA_THRESHOLD), None);
    }

    #[test]
    fn fit_fills_the_box_with_an_inset() {
        // 100 %: 16 px out, 1 px inset, 14 px room.
        assert_eq!(inset_px(16), 1.0);
        assert_eq!(inset_px(24), 2.0);
        // A full-bleed 16 px picture shrinks into the room.
        let (x, y, s) = fit(16.0, 16.0, 16.0, 16.0, 1.0);
        assert_eq!((x, y), (1.0, 1.0));
        assert!((s - 14.0 / 16.0).abs() < 1e-6);
        // A padded 12 px picture grows to the same 14 px.
        let (x, y, s) = fit(12.0, 12.0, 16.0, 16.0, 1.0);
        assert_eq!((x, y), (1.0, 1.0));
        assert!((s * 12.0 - 14.0).abs() < 1e-5);
        // Wide content: the long side fills, the short one is centered.
        let (x, y, s) = fit(14.0, 7.0, 16.0, 16.0, 1.0);
        assert_eq!(x, 1.0);
        assert!((s - 1.0).abs() < 1e-6);
        assert_eq!(y, 5.0); // (16 - 7) / 2 = 4.5, rounded
        // A 32 px source for a 16 px slot: nominal 0.5.
        let (_, _, s) = fit(28.0, 28.0, 32.0, 16.0, 1.0);
        assert!((s - 0.5).abs() < 1e-6);
        // A tiny dot never grows past MAX_GROW of its nominal size.
        let (x, _, s) = fit(4.0, 4.0, 16.0, 16.0, 1.0);
        assert!((s - MAX_GROW).abs() < 1e-6);
        assert_eq!(x, 5.0);
    }

    #[test]
    fn normalize_makes_padded_and_full_icons_the_same_size() {
        let padded = normalize(&square(16, 4, 4, 12, 12), 16, 16);
        // 8 px grows by at most 1.5x -> 12 px, centered at 2..14.
        assert_eq!(opaque_bbox(&padded, 16, 16, 128), Some((2, 2, 14, 14)));
        let full = normalize(&square(32, 0, 0, 32, 32), 32, 16);
        assert_eq!(opaque_bbox(&full, 16, 16, 128), Some((1, 1, 15, 15)));
        // Premultiplied stays valid.
        assert!(full.chunks_exact(4).all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3]));
    }
}
