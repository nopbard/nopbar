//! The built-in Pretendard through the product's own text path (gfx's
//! layouts: `ui_format`, its collection, Segoe's line box), read back by a
//! text renderer of our own instead of pixels (2026-11-11 review, P2):
//! which font each glyph run came from and whether a glyph is missing,
//! for scripts Pretendard has and ones it leaves to fallback; the line box
//! and how far the ink strays from it (the surfaces are as tall as the
//! box: ink outside it is cut); right-to-left and the ellipsis; wrapped
//! lines; and the built-in font failing to load (Windows' font instead,
//! drawn from the system's collection).

use std::cell::RefCell;

use windows::core::{Interface, Ref, BOOL, HSTRING};
use windows::Win32::Graphics::DirectWrite::*;
use windows_core::{implement, AsImpl, IUnknownImpl};

use super::*;

/// One glyph run as the layout drew it.
#[derive(Debug, Clone)]
struct Run {
    /// Its font's family (English name).
    family: String,
    /// It has a glyph the font lacks (index 0, .notdef).
    missing: bool,
    /// Right to left (an odd bidi level).
    rtl: bool,
    /// The text it draws.
    text: String,
}

#[derive(Default)]
struct Drawn {
    runs: Vec<Run>,
    /// Inline objects drawn: the ellipsis of a cut line.
    inline: Vec<bool>,
}

#[implement(IDWriteTextRenderer)]
struct Collect(RefCell<Drawn>);

impl IDWritePixelSnapping_Impl for Collect_Impl {
    fn IsPixelSnappingDisabled(&self, _: *const core::ffi::c_void) -> windows::core::Result<BOOL> {
        Ok(BOOL(1))
    }
    fn GetCurrentTransform(&self, _: *const core::ffi::c_void, transform: *mut DWRITE_MATRIX) -> windows::core::Result<()> {
        unsafe { *transform = DWRITE_MATRIX { m11: 1.0, m12: 0.0, m21: 0.0, m22: 1.0, dx: 0.0, dy: 0.0 } };
        Ok(())
    }
    fn GetPixelsPerDip(&self, _: *const core::ffi::c_void) -> windows::core::Result<f32> {
        Ok(1.0)
    }
}

impl IDWriteTextRenderer_Impl for Collect_Impl {
    fn DrawGlyphRun(&self, _: *const core::ffi::c_void, _: f32, _: f32, _: DWRITE_MEASURING_MODE, run: *const DWRITE_GLYPH_RUN, desc: *const DWRITE_GLYPH_RUN_DESCRIPTION, _: Ref<windows::core::IUnknown>) -> windows::core::Result<()> {
        unsafe {
            let run = &*run;
            let text = desc.as_ref().filter(|d| !d.string.is_null()).map(|d| String::from_utf16_lossy(std::slice::from_raw_parts(d.string.0, d.stringLength as usize))).unwrap_or_default();
            let glyphs = std::slice::from_raw_parts(run.glyphIndices, run.glyphCount as usize);
            let face = run.fontFace.as_ref().expect("a run's font");
            let family = face.cast::<IDWriteFontFace3>().ok().and_then(|f| f.GetFamilyNames().ok()).map(|n| english(&n)).unwrap_or_default();
            self.0.borrow_mut().runs.push(Run { family, missing: glyphs.contains(&0), rtl: run.bidiLevel % 2 == 1, text });
        }
        Ok(())
    }
    fn DrawUnderline(&self, _: *const core::ffi::c_void, _: f32, _: f32, _: *const DWRITE_UNDERLINE, _: Ref<windows::core::IUnknown>) -> windows::core::Result<()> {
        Ok(())
    }
    fn DrawStrikethrough(&self, _: *const core::ffi::c_void, _: f32, _: f32, _: *const DWRITE_STRIKETHROUGH, _: Ref<windows::core::IUnknown>) -> windows::core::Result<()> {
        Ok(())
    }
    fn DrawInlineObject(&self, ctx: *const core::ffi::c_void, x: f32, y: f32, object: Ref<IDWriteInlineObject>, sideways: BOOL, rtl: BOOL, effect: Ref<windows::core::IUnknown>) -> windows::core::Result<()> {
        self.0.borrow_mut().inline.push(rtl.as_bool());
        // The ellipsis draws its own glyph run through us.
        let me: IDWriteTextRenderer = self.to_interface();
        if let Some(o) = object.as_ref() {
            unsafe { o.Draw(Some(ctx), &me, x, y, sideways.as_bool(), rtl.as_bool(), effect.as_ref())? };
        }
        Ok(())
    }
}

fn english(names: &IDWriteLocalizedStrings) -> String {
    unsafe {
        let (mut i, mut found) = (0u32, BOOL(0));
        let _ = names.FindLocaleName(&HSTRING::from("en-us"), &mut i, &mut found);
        let i = if found.as_bool() { i } else { 0 };
        let len = names.GetStringLength(i).unwrap_or(0) as usize;
        let mut w = vec![0u16; len + 1];
        let _ = names.GetString(i, &mut w);
        String::from_utf16_lossy(&w[..len])
    }
}

/// What `layout` draws.
fn drawn(layout: &IDWriteTextLayout) -> Drawn {
    let c: IDWriteTextRenderer = Collect(RefCell::new(Drawn::default())).into();
    unsafe { layout.Draw(None, &c, 0.0, 0.0).unwrap() };
    let inner: &Collect = unsafe { c.as_impl() };
    let out = std::mem::take(&mut *inner.0.borrow_mut());
    out
}

fn lines(layout: &IDWriteTextLayout) -> Vec<DWRITE_LINE_METRICS> {
    unsafe {
        let mut n = 0u32;
        let _ = layout.GetLineMetrics(None, &mut n);
        let mut v = vec![DWRITE_LINE_METRICS::default(); n as usize];
        layout.GetLineMetrics(Some(&mut v), &mut n).unwrap();
        v
    }
}

fn font_size(layout: &IDWriteTextLayout) -> f32 {
    let f: &IDWriteTextFormat = layout;
    unsafe { f.GetFontSize() }
}

/// Every script NopBar's 42 languages are written in, a word each: those
/// in Pretendard (Hangul, Latin, Cyrillic, Greek, Vietnamese' marks) and
/// those its fallback takes (kana and Han, Arabic, Hebrew, Thai,
/// Devanagari). (script, text, drawn in Pretendard itself)
const SCRIPTS: [(&str, &str, bool); 12] = [
    ("ko", "설정 파일 열기", true),
    ("en", "Settings Gyp", true),
    ("ru", "Настройки", true),
    ("el", "Ρυθμίσεις", true),
    ("vi", "Cài đặt Ứng dụng", true),
    ("ja", "設定ファイル", false),
    ("zh-cn", "设置文件", false),
    ("zh-tw", "設定檔案", false),
    ("ar", "الإعدادات", false),
    ("he", "הגדרות", false),
    ("th", "การตั้งค่าที่ปุ่ม", false),
    ("hi", "सेटिंग्स", false),
];

/// Text in each script, laid out as the product lays out a label (a line
/// with "…": gfx::line_layout) in the built-in font: no glyph missing,
/// Pretendard's own scripts in Pretendard, the others in a fallback font
/// (never a box), on Segoe's line box, and its ink inside the box the
/// surface is made of (above its top, below its foot), within 1 px.
#[test]
fn every_script_draws_in_the_built_in_box() {
    let _l = crate::i18n::test_lock("en");
    let dw: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap() };
    let (line_em, base_em) = bundled_line_box();
    for scale in [1.0f32, 1.5, 2.0] {
        for (tag, text, own) in SCRIPTS {
            let (layout, w, h) = crate::gfx::line_layout(&dw, text, BUNDLED, 13.0 * scale, DWRITE_FONT_WEIGHT_NORMAL, 4000.0).unwrap();
            let size = font_size(&layout);
            let d = drawn(&layout);
            assert!(!d.runs.is_empty(), "{tag}: drawn");
            for r in &d.runs {
                assert!(!r.missing, "{tag} {text:?}: a glyph missing in {:?} ({:?})", r.family, r.text);
            }
            // Pretendard's scripts in it alone; the others reach a
            // fallback font (Japanese: its kana are Pretendard's own, its
            // Han the fallback's).
            if own {
                assert!(d.runs.iter().all(|r| r.family == BUNDLED_FAMILY), "{tag} {text:?}: {:?}", d.runs);
            } else {
                assert!(d.runs.iter().any(|r| r.family != BUNDLED_FAMILY && !r.family.is_empty()), "{tag} {text:?}: no fallback {:?}", d.runs);
            }
            let ls = lines(&layout);
            assert_eq!(ls.len(), 1, "{tag}: one line");
            assert!((ls[0].height - line_em * size).abs() < 0.01, "{tag}: Segoe's line box {} = {}", ls[0].height, line_em * size);
            assert!((ls[0].baseline - base_em * size).abs() < 0.01, "{tag}: its baseline");
            assert!(w > 0.0 && h >= ls[0].height.floor(), "{tag}: {w} x {h}");
            // The ink (overhangs: + is outside the layout's box, whose
            // foot is its max height) inside the surface's h.
            let o = unsafe { layout.GetOverhangMetrics().unwrap() };
            let max_h = unsafe { layout.GetMaxHeight() };
            let (ink_top, ink_foot) = (-o.top, max_h + o.bottom);
            assert!(ink_top >= -1.0, "{tag} {text:?} @{scale}: ink {:.2} px over the box's top", -ink_top);
            assert!(ink_foot <= h + 1.0, "{tag} {text:?} @{scale}: ink {:.2} px under the surface ({h})", ink_foot - h);
        }
    }
}

/// Right to left (Arabic and Hebrew UI) in the built-in font: the line
/// reads right to left, a long one is cut at its room with "…" (drawn
/// right to left, inside the room), wrapped text keeps Segoe's line box on
/// every line and stays in its width.
#[test]
fn right_to_left_cuts_and_wraps_in_the_built_in_font() {
    let stage = crate::gfx::promo_kit::stage();
    let gfx = &stage.gfx;
    let (line_em, _) = bundled_line_box();
    for (tag, long) in [("ar", "إعدادات شريط المهام وقائمة ابدأ والإشعارات"), ("he", "הגדרות שורת המשימות ותפריט התחל וההודעות")] {
        let _l = crate::i18n::test_lock(tag);
        assert!(crate::i18n::rtl(), "{tag}");
        let dw = gfx.dwrite();
        let room = 120.0;
        let (layout, w, _) = crate::gfx::line_layout(dw, long, BUNDLED, 26.0, DWRITE_FONT_WEIGHT_NORMAL, room).unwrap();
        let f: &IDWriteTextFormat = &layout;
        assert_eq!(unsafe { f.GetReadingDirection() }, DWRITE_READING_DIRECTION_RIGHT_TO_LEFT, "{tag}");
        assert!(w <= room, "{tag}: {w} in {room}");
        let d = drawn(&layout);
        assert_eq!(d.inline, [true], "{tag}: one ellipsis, right to left");
        assert!(d.runs.iter().any(|r| r.rtl), "{tag}: right-to-left runs");
        assert!(d.runs.iter().all(|r| !r.missing), "{tag}: {:?}", d.runs);
        let ls = lines(&layout);
        assert_eq!(ls.len(), 1);
        // Wrapped (Launchpad's names, Settings' hints): two lines at most,
        // each Segoe's box, inside the width.
        let (wrapped, h) = gfx.text_layout_wrapped(long, BUNDLED, 26.0, DWRITE_FONT_WEIGHT_NORMAL, 200.0, 2).unwrap();
        let size = font_size(&wrapped);
        // (Lines past the two the box holds are laid out with no height.)
        let ls: Vec<DWRITE_LINE_METRICS> = lines(&wrapped).into_iter().filter(|l| l.height > 0.0).collect();
        assert_eq!(ls.len(), 2, "{tag}: wraps to two lines");
        for l in &ls {
            assert!((l.height - line_em * size).abs() < 0.01, "{tag}: each line on Segoe's box ({})", l.height);
        }
        assert!(h <= (2.0 * line_em * size).ceil() + 0.5, "{tag}: two lines' height {h}");
        let mut m = DWRITE_TEXT_METRICS::default();
        unsafe { wrapped.GetMetrics(&mut m).unwrap() };
        assert!(m.width <= 200.0 + 0.01, "{tag}: in its width {}", m.width);
        assert!(drawn(&wrapped).runs.iter().all(|r| !r.missing));
    }
}

/// The built-in font failing to load: bytes DirectWrite cannot read are
/// an error, not a collection; and on a thread where it failed the UI font
/// is Windows', laid out from the system's collection with its own line
/// spacing (not Segoe's box: that is Pretendard's), with nothing missing.
#[test]
fn a_built_in_font_that_fails_is_windows_font() {
    static NOT_A_FONT: [u8; 64] = [7; 64];
    assert!(load(&NOT_A_FONT).is_err(), "unreadable bytes: no collection");
    assert!(load(BUNDLED_TTF).is_ok(), "the real one loads again (a new loader each time)");
    let _l = crate::i18n::test_lock("ko");
    FAIL_BUNDLED.with(|f| f.set(true));
    let result = std::panic::catch_unwind(|| {
        assert!(bundled().is_none() && !installed(BUNDLED) && collection_for(BUNDLED).is_none());
        let face = pick("", THEME_DEFAULT, &system_face(), installed);
        assert_ne!(face, BUNDLED, "Windows' font");
        assert!(installed(&face), "{face}: on the PC");
        let dw: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap() };
        let (layout, w, _) = crate::gfx::line_layout(&dw, "설정 Settings", &face, 26.0, DWRITE_FONT_WEIGHT_NORMAL, 4000.0).unwrap();
        assert!(w > 0.0);
        let d = drawn(&layout);
        assert!(d.runs.iter().all(|r| !r.missing && r.family != BUNDLED_FAMILY), "{face}: {:?}", d.runs);
        let f: &IDWriteTextFormat = &layout;
        let (mut method, mut spacing, mut base) = (DWRITE_LINE_SPACING_METHOD::default(), 0.0f32, 0.0f32);
        unsafe { f.GetLineSpacing(&mut method, &mut spacing, &mut base).unwrap() };
        assert_eq!(method, DWRITE_LINE_SPACING_METHOD_DEFAULT, "its own line spacing");
    });
    FAIL_BUNDLED.with(|f| f.set(false));
    result.unwrap();
    assert!(bundled().is_some(), "only that thread saw it fail");
}
