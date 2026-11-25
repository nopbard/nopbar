//! Where the bar's text stands: one baseline per box (the bar, the
//! island's pill), from the font's own metrics, so the text's optical band
//! is centered in it, on a whole pixel.
//!
//! A text layout's line box is not centered on its letters: it holds the
//! font's whole ascent above the baseline (Segoe UI: 1.08 em, the caps only
//! 0.70 em of it) and its descent below (0.25 em). Centering that box put
//! the letters about 0.06 em low, plus up to half a pixel from rounding the
//! box's height up, plus a half pixel at 125 / 150 / 175 % from rounding
//! the offset in DIPs. Every text cell now stands on the box's baseline
//! instead, whatever font its letters fall back to (Hangul in Malgun
//! Gothic, kana in Yu Gothic UI): neighbouring cells line up.

use crate::gfx::TextLine;

/// A font's vertical metrics in its design units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub em: f32,
    pub descent: f32,
    pub cap: f32,
}

impl Metrics {
    /// Segoe UI (and Segoe UI Variable): what a font that cannot be read
    /// stands in for.
    pub const SEGOE: Metrics = Metrics { em: 2048.0, descent: 514.0, cap: 1434.0 };

    /// From DirectWrite's DWRITE_FONT_METRICS values (designUnitsPerEm,
    /// descent, capHeight). A font without a cap height gets 0.7 em.
    pub fn of(em: u16, descent: u16, cap: u16) -> Metrics {
        if em == 0 {
            return Metrics::SEGOE;
        }
        let em = em as f32;
        let cap = if cap == 0 { 0.7 * em } else { cap as f32 };
        Metrics { em, descent: descent as f32, cap }
    }

    /// How far above the baseline the text's optical middle is, for the
    /// font at `size_px`: the middle of the band from a tenth of the
    /// descent below the baseline to 1.08 cap heights above it. The caps of
    /// Latin, digits and Arabic sit from the baseline to the cap height;
    /// Hangul and kana (Malgun Gothic, Yu Gothic) run past both (Malgun's
    /// Hangul: 0.09 em below to 0.85 em above). The band is the middle
    /// ground: Latin caps a fifth of a pixel above the middle at 13 px,
    /// Hangul about as much below it (the eye puts a box's middle a little
    /// high anyway).
    pub fn optical_mid(&self, size_px: f32) -> f32 {
        0.5 * (1.08 * self.cap - 0.1 * self.descent) / self.em * size_px
    }
}

/// The baseline, px from the top of a box `box_px` tall, that puts the
/// optical middle of text `size_px` tall in `m` at the box's middle, on a
/// whole pixel (the text draws crisp: Direct2D snaps its baseline to the
/// pixel grid).
pub fn baseline(box_px: f32, m: &Metrics, size_px: f32) -> f32 {
    (box_px / 2.0 + m.optical_mid(size_px)).round()
}

/// The line for a box `box_px` tall (rounded to whole pixels).
pub fn line(box_px: f32, m: &Metrics, size_px: f32) -> TextLine {
    let box_h = box_px.round().max(1.0);
    TextLine { box_h, baseline: baseline(box_h, m, size_px) }
}

/// Where to draw a layout (its top, px from the box's top) whose first line
/// has its baseline `line_baseline` px below the layout's top, so that line
/// stands on `baseline`.
pub fn origin_y(baseline: f32, line_baseline: f32) -> f32 {
    baseline - line_baseline
}

/// The top of `content_px` centered in `box_px`, on a whole pixel.
pub fn centered_px(box_px: f32, content_px: f32) -> f32 {
    ((box_px - content_px) / 2.0).round()
}

/// The DIP offset of a cell `content` DIP tall centered in a bar `bar` DIP
/// tall at `scale`: whole pixels, not whole DIPs (one DIP is 1.5 px at
/// 150 %: a whole-DIP offset there put cells on half pixels, blurred).
pub fn cell_top(bar: f32, content: f32, scale: f32) -> f32 {
    let scale = if scale > 0.0 { scale } else { 1.0 };
    centered_px((bar * scale).round(), (content * scale).round()) / scale
}

/// `v` DIP on the nearest device pixel at `scale` (an x on the bar: whole
/// pixels, not whole DIPs).
pub fn snap_px(v: f32, scale: f32) -> f32 {
    let scale = if scale > 0.0 { scale } else { 1.0 };
    (v * scale).round() / scale
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Malgun Gothic (DWRITE_FONT_METRICS, as installed).
    const MALGUN: Metrics = Metrics { em: 2048.0, descent: 495.0, cap: 1471.0 };
    /// Yu Gothic UI.
    const YU: Metrics = Metrics { em: 2048.0, descent: 514.0, cap: 1581.0 };
    /// Pretendard.
    const PRETENDARD: Metrics = Metrics { em: 2048.0, descent: 494.0, cap: 1448.0 };

    const SCALES: [f32; 5] = [1.0, 1.25, 1.5, 1.75, 2.0];

    /// The bar's text at 13 DIP (the theme's size) and the settings' text
    /// sizes around it.
    const SIZES: [f32; 4] = [11.0, 13.0, 14.3, 15.6];

    #[test]
    fn the_optical_middle_lands_on_the_bar_middle() {
        for m in [Metrics::SEGOE, MALGUN, YU, PRETENDARD] {
            for scale in SCALES {
                for bar in [20.0f32, 24.0, 28.0, 30.0, 32.0, 36.0, 40.0, 48.0] {
                    for size in SIZES {
                        let (h, px) = ((bar * scale).round(), size * scale);
                        let b = baseline(h, &m, px);
                        let mid = b - m.optical_mid(px);
                        assert!((mid - h / 2.0).abs() <= 0.5 + 1e-4, "{m:?} {bar} DIP at {scale}: middle {mid}, bar {}", h / 2.0);
                    }
                }
            }
        }
    }

    #[test]
    fn the_baseline_is_on_a_whole_pixel() {
        for scale in SCALES {
            for bar in [28.0f32, 29.0, 30.0, 31.0, 32.0] {
                let l = line(bar * scale, &Metrics::SEGOE, 13.0 * scale);
                assert_eq!(l.baseline, l.baseline.round());
                assert_eq!(l.box_h, l.box_h.round());
                assert!(l.baseline > 0.0 && l.baseline < l.box_h);
            }
        }
    }

    /// Latin in the bar's font and Hangul falling back to Malgun Gothic:
    /// the two layouts' first lines sit at different depths (DirectWrite:
    /// 14.03 and 14.15 px at 13 px), yet both stand on the one baseline.
    #[test]
    fn latin_and_hangul_cells_share_the_baseline() {
        let l = line(32.0, &Metrics::SEGOE, 13.0);
        let (latin, hangul) = (14.03f32, 14.15f32);
        let (top_latin, top_hangul) = (origin_y(l.baseline, latin), origin_y(l.baseline, hangul));
        assert!((top_latin + latin - l.baseline).abs() < 1e-4);
        assert!((top_hangul + hangul - l.baseline).abs() < 1e-4);
        // Their ink (measured: Latin caps 0..9.10 px, Hangul -1.14..11.10
        // px about the baseline) is centered within a quarter of a pixel
        // either side of the optical middle.
        let mid = l.baseline - Metrics::SEGOE.optical_mid(13.0);
        let (latin_mid, hangul_mid) = (l.baseline - 9.10 / 2.0, l.baseline - (11.10 - 1.14) / 2.0);
        assert!((latin_mid - mid).abs() < 0.25 && (hangul_mid - mid).abs() < 0.25, "{latin_mid} {hangul_mid} {mid}");
        // At 150 % (19.5 px: caps 13.65, Hangul -1.71..16.65) too, within a
        // third of a pixel.
        let mid = 28.0 - Metrics::SEGOE.optical_mid(19.5);
        assert!((28.0 - 13.65 / 2.0 - mid).abs() < 0.35 && (28.0 - (16.65 - 1.71) / 2.0 - mid).abs() < 0.35);
    }

    /// The numbers in the report: a 28 and a 32 DIP bar at 100 % and 150 %.
    #[test]
    fn known_bars() {
        let seg = Metrics::SEGOE;
        assert_eq!(baseline(28.0, &seg, 13.0), 19.0);
        assert_eq!(baseline(32.0, &seg, 13.0), 21.0);
        assert_eq!(baseline(42.0, &seg, 19.5), 28.0);
        assert_eq!(baseline(48.0, &seg, 19.5), 31.0);
        assert_eq!(baseline(28.0, &MALGUN, 13.0), 19.0);
        assert_eq!(baseline(42.0, &MALGUN, 19.5), 28.0);
    }

    /// The real fonts (Segoe UI, Malgun Gothic as the bar's font): Latin,
    /// digits, Hangul (falling back to Malgun Gothic), kana and Arabic laid
    /// out by DirectWrite and drawn on the line stand on one baseline, their
    /// ink centered in the bar (within a pixel: the snap plus how far each
    /// script's ink strays from the band), at every scale.
    #[test]
    fn real_layouts_line_up_in_the_bar() {
        use windows::core::{w, HSTRING};
        use windows::Win32::Graphics::DirectWrite::*;
        unsafe {
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap();
            let mut coll = None;
            dw.GetSystemFontCollection(&mut coll, false).unwrap();
            let coll = coll.unwrap();
            for family in ["Segoe UI", "Malgun Gothic"] {
                let (mut i, mut exists) = (0u32, windows::core::BOOL(0));
                coll.FindFamilyName(&HSTRING::from(family), &mut i, &mut exists).unwrap();
                if !exists.as_bool() {
                    eprintln!("{family} is not installed: skipped");
                    continue;
                }
                let font = coll.GetFontFamily(i).unwrap().GetFirstMatchingFont(DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL).unwrap();
                let mut fm = DWRITE_FONT_METRICS::default();
                font.GetMetrics(&mut fm);
                let m = Metrics::of(fm.designUnitsPerEm, fm.descent, fm.capHeight);
                for scale in SCALES {
                    for bar in [28.0f32, 32.0] {
                        let size = 13.0 * scale;
                        let l = line(bar * scale, &m, size);
                        let format = dw.CreateTextFormat(&HSTRING::from(family), None, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!("")).unwrap();
                        for text in ["File", "12:34", "파일", "오후 3:45", "ファイル", "ملف"] {
                            let wide: Vec<u16> = text.encode_utf16().collect();
                            let layout = dw.CreateTextLayout(&wide, &format, 4000.0, 400.0).unwrap();
                            let mut tm = DWRITE_TEXT_METRICS::default();
                            layout.GetMetrics(&mut tm).unwrap();
                            let first = crate::gfx::first_baseline(&layout, tm.height);
                            let y = origin_y(l.baseline, first);
                            assert!((y + first - l.baseline).abs() < 1e-3, "{family} {text}: on the line");
                            // Its ink is centered in the bar: within the snap
                            // (half a pixel) and how far each script strays
                            // from the band (0.03 em).
                            let o = layout.GetOverhangMetrics().unwrap();
                            let (top, bottom) = (y - o.top, y + tm.layoutHeight + o.bottom);
                            let mid = (top + bottom) / 2.0;
                            let tol = 0.5 + 0.03 * size;
                            assert!((mid - l.box_h / 2.0).abs() <= tol, "{family} {text} {bar} DIP at {scale}: ink {top}..{bottom}, mid {mid}, bar {}", l.box_h / 2.0);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_font_without_metrics_stands_in_for_segoe() {
        assert_eq!(Metrics::of(0, 0, 0), Metrics::SEGOE);
        assert_eq!(Metrics::of(2048, 514, 1434), Metrics::SEGOE);
        let m = Metrics::of(1000, 200, 0);
        assert_eq!(m.cap, 700.0);
    }

    #[test]
    fn cells_sit_on_whole_pixels() {
        for scale in SCALES {
            for (bar, content) in [(28.0f32, 20.0f32), (32.0, 17.0), (30.0, 22.0 / 1.5), (32.0, 32.0)] {
                let top = cell_top(bar, content, scale) * scale;
                assert!((top - top.round()).abs() < 1e-3, "{bar} {content} {scale}: {top}");
            }
        }
        // A cell as tall as the bar (the text cells) sits at its top.
        assert_eq!(cell_top(28.0, 42.0 / 1.5, 1.5), 0.0);
        assert_eq!(cell_top(30.5, 38.0 / 1.25, 1.25), 0.0);
        // At 100 % nothing moves from the whole-DIP rounding.
        assert_eq!(cell_top(32.0, 20.0, 1.0), 6.0);
    }
}
