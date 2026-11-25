//! The font comparison (tests only): the same surfaces drawn in each of
//! the UI fonts Settings offers, by the product's own path
//! (`appearance.font`, crate::fonts::pick):
//!
//! - `pretendard`: the default, Pretendard built into the exe
//!   (fonts::bundled: its own collection, never installed), on Segoe UI's
//!   line box (fonts::segoe_line_box) and with its own tracking
//!   (fonts::tracking);
//! - `windows`: Windows' UI font (`appearance.font = "system"`: the message
//!   font; on a Korean Windows that is Malgun Gothic, Latin included), the
//!   default before Pretendard;
//! - `segoe`: Segoe UI Variable (the family the type ramp was drawn with;
//!   Korean falls back to Malgun Gothic).
//!
//! The surfaces: the top bar (the film's, Korean and English), the dock's
//! labels, the dock's menus (popup), the PC status panel, two Settings
//! pages and a Launchpad page, light and dark, at 2x. Pictures go to
//! `$FONT_COMPARE_DIR/<face>/<light|dark>/` (default
//! `target/font-compare/`):
//!
//! ```text
//! cargo test -p dock font_compare -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};

use windows::core::HSTRING;
use windows::Win32::Graphics::DirectWrite::{DWriteCreateFactory, DWRITE_FACTORY_TYPE_SHARED};

use crate::gfx::promo_kit as kit;

/// The surfaces drawn on promo_kit's stage (its dispatcher queue).
fn stage_scenes() {
    crate::topbar::font_compare();
    crate::dock::font_compare_label();
    crate::dock::font_compare_menu();
    crate::topbar::stats::font_compare();
    crate::launchpad::font_compare();
}

/// `scenes` on a thread of their own, into `root/<face>`. Settings'
/// harness makes its own dispatcher queue, which a thread can have only
/// one of: it gets a thread apart from the stage's.
fn on_thread(root: &Path, face: &'static str, scenes: fn()) {
    let dir = root.join(face);
    std::thread::spawn(move || kit::with_shot_root(dir, scenes))
    .join()
    .unwrap_or_else(|_| panic!("{face}: a scene failed"));
}

/// Words measured in each face (body 13 DIP, regular), printed as a table.
const WORDS: [&str; 8] = ["파일 편집 보기 이동 창 도움말", "Files Edit View Go Window Help", "Visual Studio Code", "카카오톡", "한컴오피스 한글 2024", "11월 9일 (월) 오후 2:30", "작업 표시줄 숨기기", "0123456789"];

/// The width (DIP) and the line's height of each of WORDS at 13 DIP in
/// the UI font now.
fn measure() -> Vec<(f32, f32)> {
    use windows::Win32::Graphics::DirectWrite::{IDWriteFactory, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_METRICS};
    let dw: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap() };
    let fam = HSTRING::from(&*crate::fonts::face_for(&crate::fonts::ui_family(), 13.0));
    let f = crate::gfx::ui_format(&dw, &fam, DWRITE_FONT_WEIGHT_NORMAL, 13.0).unwrap();
    WORDS
        .iter()
        .map(|t| unsafe {
            let w: Vec<u16> = t.encode_utf16().collect();
            let l = dw.CreateTextLayout(&w, &f, 4000.0, 400.0).unwrap();
            let mut m = DWRITE_TEXT_METRICS::default();
            l.GetMetrics(&mut m).unwrap();
            (m.width, m.height)
        })
        .collect()
}

/// The faces, by folder, with the `appearance.font` each is drawn with
/// ("" = the default, Pretendard).
const FACES: [(&str, &str); 3] = [("pretendard", ""), ("windows", crate::fonts::SYSTEM_CHOICE), ("segoe", crate::fonts::THEME_DEFAULT)];

#[test]
#[ignore = "pictures: cargo test -p dock font_compare -- --ignored --nocapture"]
fn font_compare() {
    let root = std::env::var_os("FONT_COMPARE_DIR").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("target").join("font-compare"));
    let mut widths = Vec::new();
    for (face, font) in FACES {
        // The font is process-wide (Theme::resolve reads it): set for each
        // face, the default again after.
        crate::fonts::apply(font, "");
        println!("font_compare: {face}: ui_family {:?}", crate::fonts::ui_family());
        on_thread(&root, face, stage_scenes);
        on_thread(&root, face, crate::settings::font_compare);
        let got = std::thread::spawn(measure).join().unwrap();
        widths.push((face, got));
    }
    println!("font_compare: width / line height at 13 DIP");
    for (i, word) in WORDS.iter().enumerate() {
        let row: Vec<String> = widths.iter().map(|(face, v)| format!("{face} {:.1} / {:.1}", v[i].0, v[i].1)).collect();
        println!("font_compare: {word:?}: {}", row.join(", "));
    }
    crate::fonts::apply("", "");
    // Pretendard's lines stand in Segoe's box: every word's line as tall
    // as in Segoe UI Variable (Korean there falls back to Malgun Gothic,
    // a little taller), and its width near Segoe's (Pretendard is wider
    // in digits, narrower in Hangul): within a quarter.
    let of = |face: &str| &widths.iter().find(|(f, _)| *f == face).unwrap().1;
    let (pre, seg) = (of("pretendard"), of("segoe"));
    for (i, word) in WORDS.iter().enumerate() {
        let ((pw, ph), (sw, sh)) = (pre[i], seg[i]);
        assert!((ph - sh).abs() <= 0.2, "{word:?}: line {ph} against Segoe's {sh}");
        assert!((0.75..=1.25).contains(&(pw / sw)), "{word:?}: width {pw} against Segoe's {sw}");
    }
    // Each face took: the same picture differs between them.
    for name in ["dock-label", "topbar-wide-ko", "dock-menu-app", "stats-busy", "settings-general", "launchpad-wide"] {
        let read = |face: &str| std::fs::read(root.join(face).join("light").join(format!("{name}.png"))).unwrap_or_else(|e| panic!("{face}/{name}: {e}"));
        let (win, seg, pre) = (read("windows"), read("segoe"), read("pretendard"));
        assert!(seg != pre, "{name}: Segoe and Pretendard drawn the same");
        assert!(win != pre, "{name}: Windows' font and Pretendard drawn the same");
    }
    println!("font_compare: pictures under {}", root.display());
}
