//! The UI font and the text size, process-wide.
//!
//! By default NopBar draws in Pretendard (BUNDLED), built into nopbar.exe
//! and read from memory into a font collection of its own: it is never
//! installed on the PC. Settings > Appearance can pick Windows' UI font
//! instead (SYSTEM_CHOICE: the message font, SPI_GETNONCLIENTMETRICS,
//! re-read on WM_SETTINGCHANGE through syslook::current()) or any installed
//! family (`appearance.font`), and a text size (`appearance.text_size`).
//! Characters the family lacks (kana and Han in Pretendard, Korean in a
//! Latin font, ...) still come from DirectWrite's font fallback.
//!
//! Everything that makes text goes through gfx (or calls `map` and `scale`
//! itself), so a change reaches every surface at once: the theme carries the
//! resolved family and scale (Theme::resolve), so a change also counts as a
//! new theme and surfaces drop what they drew with the old one.

use std::sync::RwLock;

/// The family Graphite names (and the type tokens were drawn with): it
/// stands for "the UI font", whatever that is now.
pub const THEME_DEFAULT: &str = "Segoe UI Variable Text";

/// The text sizes (`appearance.text_size`), by option index, and how much
/// each scales every type token (theme::ramp: headline 15, body 13, footnote 12, caption 11,
/// the top bar's 12, ...).
pub const TEXT_SIZES: [&str; 3] = ["small", "default", "large"];
pub const TEXT_SCALES: [f32; 3] = [0.92, 1.0, 1.1];

/// Shown first in the font list when installed (by their English names),
/// after Pretendard (the default) and Windows' UI font.
pub const RECOMMENDED: [&str; 5] = ["Segoe UI Variable Text", "Segoe UI", "Malgun Gothic", "Noto Sans KR", "Inter"];

/// The built-in Pretendard as a family to draw with (what `pick` returns
/// and the theme carries): a name no installed family has, so the built-in
/// font and a Pretendard installed on the PC never stand for each other
/// (2026-11-11 review). gfx turns it into BUNDLED_FAMILY in the built-in
/// collection (`collection_for`, `dwrite_name`).
pub const BUNDLED: &str = "NopBar Pretendard";

/// Pretendard's family in the built-in font file (PretendardVariable.ttf
/// v1.3.9, SIL OFL 1.1: assets/fonts, THIRD_PARTY_NOTICES.md). The same
/// name on the PC is an installed Pretendard: the user's, never this one.
pub const BUNDLED_FAMILY: &str = "Pretendard Variable";

/// `appearance.font` for Windows' UI font (the default before Pretendard:
/// the message font, Segoe UI Variable for stock Segoe UI).
pub const SYSTEM_CHOICE: &str = "system";

/// The built-in font file: in the exe, so the installer, the portable zip
/// and every automatic update carry it with nothing else to copy.
static BUNDLED_TTF: &[u8] = include_bytes!("../assets/fonts/PretendardVariable.ttf");

/// The text size option a value stands for (unknown or empty: Default).
pub fn size_index(v: &str) -> usize {
    TEXT_SIZES.iter().position(|s| s.eq_ignore_ascii_case(v.trim())).unwrap_or(1)
}

/// The scale a text size value means.
pub fn scale_of(v: &str) -> f32 {
    TEXT_SCALES[size_index(v)]
}

/// The family to draw with. `choice` is the user's (`appearance.font`, ""
/// = none: the built-in Pretendard, SYSTEM_CHOICE = Windows' UI font, else
/// an installed family by name), `theme` the theme's own family, `system`
/// Windows' message font. Windows' font, asked for, is Windows' font
/// whatever the theme names (2026-11-11 review). Otherwise the user's
/// family wins, then a theme's own family, then Pretendard (built in;
/// `has(BUNDLED)` is false only when it cannot be read), then the system's.
/// A family named in `choice` is an installed one, a Pretendard too: the
/// built-in font is only ever "" (BUNDLED). Families that are not
/// installed are passed over.
pub fn pick(choice: &str, theme: &str, system: &str, has: impl Fn(&str) -> bool) -> String {
    let choice = choice.trim();
    if choice.eq_ignore_ascii_case(SYSTEM_CHOICE) {
        return windows_font(system, &has);
    }
    if !choice.is_empty() && has(choice) {
        return choice.to_string();
    }
    let theme = theme.trim();
    if !theme.is_empty() && !theme.eq_ignore_ascii_case(THEME_DEFAULT) && has(theme) {
        return theme.to_string();
    }
    if has(BUNDLED) {
        return BUNDLED.to_string();
    }
    windows_font(system, &has)
}

/// Pure: Windows' UI font for message font `system`. Windows 11 still
/// reports plain "Segoe UI" while its shell draws in Segoe UI Variable:
/// that stock face means the Variable one when installed.
fn windows_font(system: &str, has: &impl Fn(&str) -> bool) -> String {
    let system = system.trim();
    let stock = system.is_empty() || system.eq_ignore_ascii_case("Segoe UI");
    if stock && has(THEME_DEFAULT) {
        return THEME_DEFAULT.to_string();
    }
    if !system.is_empty() && has(system) {
        return system.to_string();
    }
    if has(THEME_DEFAULT) {
        THEME_DEFAULT.to_string()
    } else {
        "Segoe UI".to_string()
    }
}

/// The font list for Settings: (family, shown name) pairs, the installed
/// recommended ones first (in RECOMMENDED's order), then every other family
/// by its shown name, without duplicates. An installed Pretendard is in it
/// under its own name (the built-in one is the list's default, above
/// these). `all` is every installed family as (English name, localized
/// name).
pub fn menu_list(all: &[(String, String)]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for r in RECOMMENDED {
        if let Some(f) = all.iter().find(|(en, _)| en.eq_ignore_ascii_case(r)) {
            out.push(f.clone());
        }
    }
    let mut rest: Vec<(String, String)> = all.iter().filter(|(en, _)| !out.iter().any(|(o, _)| o.eq_ignore_ascii_case(en))).cloned().collect();
    rest.sort_by_key(|(en, shown)| (shown.to_lowercase(), en.to_lowercase()));
    rest.dedup_by(|a, b| a.0.eq_ignore_ascii_case(&b.0));
    // Vertical (@) and symbol-only families are of no use for UI text.
    rest.retain(|(en, _)| !en.starts_with('@'));
    out.extend(rest);
    out
}

struct State {
    /// The user's family ("" = Pretendard, SYSTEM_CHOICE = Windows' font).
    choice: String,
    /// Windows' message font, as last read (None: not read yet).
    system: Option<String>,
    scale: f32,
}

static STATE: RwLock<State> = RwLock::new(State { choice: String::new(), system: None, scale: 1.0 });

/// Takes the user's font and text size (Settings, a config reload).
/// Returns whether anything changed.
pub fn apply(font: &str, text_size: &str) -> bool {
    let Ok(mut s) = STATE.write() else { return false };
    let (choice, scale) = (font.trim().to_string(), scale_of(text_size));
    let changed = s.choice != choice || s.scale != scale;
    s.choice = choice;
    s.scale = scale;
    changed
}

/// Reads Windows' UI font again (startup, WM_SETTINGCHANGE). Returns a
/// hash of it, so a change shows in SysLook.
pub fn refresh_system() -> u64 {
    let face = system_face();
    let hash = crate::icons::fnv64(face.as_bytes());
    if let Ok(mut s) = STATE.write() {
        s.system = Some(face);
    }
    hash
}

/// The family to draw with for a theme naming `theme_family`.
pub fn family_for(theme_family: &str) -> String {
    let (choice, system) = match STATE.read() {
        Ok(s) => (s.choice.clone(), s.system.clone()),
        Err(_) => (String::new(), None),
    };
    let system = system.unwrap_or_else(|| {
        refresh_system();
        STATE.read().ok().and_then(|s| s.system.clone()).unwrap_or_default()
    });
    pick(&choice, theme_family, &system, installed)
}

/// The face GDI (a RichEdit) knows `family` by: the built-in Pretendard
/// is added for this process only (AddFontMemResourceEx, once) under its
/// file's family name; anything else is installed already.
pub fn gdi_face(family: &str) -> String {
    if !is_builtin(family) {
        return family.to_string();
    }
    static ADDED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let ok = *ADDED.get_or_init(|| {
        // A file of this process's (FR_PRIVATE: no other program sees it).
        let path = std::env::temp_dir().join(format!("nopbar-pretendard-{}.ttf", BUNDLED_TTF.len()));
        if std::fs::metadata(&path).map(|m| m.len() as usize).ok() != Some(BUNDLED_TTF.len()) && std::fs::write(&path, BUNDLED_TTF).is_err() {
            return false;
        }
        let wide = windows::core::HSTRING::from(path.as_os_str());
        unsafe { windows::Win32::Graphics::Gdi::AddFontResourceExW(&wide, windows::Win32::Graphics::Gdi::FR_PRIVATE, None) > 0 }
    });
    if ok {
        BUNDLED_FAMILY.to_string()
    } else {
        family.to_string()
    }
}

/// The UI font now.
pub fn ui_family() -> String {
    family_for(THEME_DEFAULT)
}

/// What gfx draws `family` with: THEME_DEFAULT stands for the UI font
/// (places that name it directly: Settings, labels, badges), anything else
/// is kept.
pub fn map(family: &str) -> std::borrow::Cow<'_, str> {
    if family.eq_ignore_ascii_case(THEME_DEFAULT) {
        std::borrow::Cow::Owned(ui_family())
    } else {
        std::borrow::Cow::Borrowed(family)
    }
}

/// Segoe UI Variable's named instances (optical size 8, 10.5 and 36 pt:
/// spike S2), and the family they share.
const VARIABLE: &str = "Segoe UI Variable";
const FACES: [(crate::theme::ramp::Face, &str); 3] = [
    (crate::theme::ramp::Face::Small, "Segoe UI Variable Small"),
    (crate::theme::ramp::Face::Text, "Segoe UI Variable Text"),
    (crate::theme::ramp::Face::Display, "Segoe UI Variable Display"),
];

/// Pure: the instance for text `size` DIP (before the display's scale):
/// Small up to 11, Text for 12 to 19, Display from 20 (design system v2.1,
/// 4.1: the named instances, not a continuous opsz).
pub fn face_of(size: f32) -> crate::theme::ramp::Face {
    use crate::theme::ramp::Face;
    if size < 11.5 {
        Face::Small
    } else if size < 20.0 {
        Face::Text
    } else {
        Face::Display
    }
}

/// The family to draw `family` with at `size` DIP: Segoe UI Variable's
/// instance for that size when `family` is one of them (the default UI
/// font); any other family (the user's choice, Windows' own) as it is.
pub fn face_for(family: &str, size: f32) -> std::borrow::Cow<'_, str> {
    let f = family.trim();
    if !(f.len() >= VARIABLE.len() && f[..VARIABLE.len()].eq_ignore_ascii_case(VARIABLE)) {
        return std::borrow::Cow::Borrowed(family);
    }
    let face = face_of(size);
    let name = FACES.iter().find(|(k, _)| *k == face).map_or(THEME_DEFAULT, |(_, n)| n);
    std::borrow::Cow::Borrowed(name)
}

/// The text size factor now.
pub fn scale() -> f32 {
    STATE.read().map(|s| s.scale).unwrap_or(1.0)
}

/// Windows' message font face ("" if it cannot be read).
fn system_face() -> String {
    use windows::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS};
    let mut m = NONCLIENTMETRICSW { cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32, ..Default::default() };
    let ok = unsafe { SystemParametersInfoW(SPI_GETNONCLIENTMETRICS, m.cbSize, Some(&mut m as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)).is_ok() };
    if !ok {
        return String::new();
    }
    let face = &m.lfMessageFont.lfFaceName;
    let n = face.iter().position(|&c| c == 0).unwrap_or(face.len());
    String::from_utf16_lossy(&face[..n])
}

thread_local! {
    static COLLECTION: std::cell::OnceCell<Option<windows::Win32::Graphics::DirectWrite::IDWriteFontCollection>> = const { std::cell::OnceCell::new() };
}

fn with_collection<R>(f: impl FnOnce(&windows::Win32::Graphics::DirectWrite::IDWriteFontCollection) -> R) -> Option<R> {
    use windows::Win32::Graphics::DirectWrite::{DWriteCreateFactory, IDWriteFactory, DWRITE_FACTORY_TYPE_SHARED};
    COLLECTION.with(|c| {
        let coll = c.get_or_init(|| unsafe {
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).ok()?;
            let mut coll = None;
            dw.GetSystemFontCollection(&mut coll, false).ok()?;
            coll
        });
        coll.as_ref().map(f)
    })
}

/// The family can be drawn: installed (any of its names), or the built-in
/// Pretendard (BUNDLED) when it could be read.
pub fn installed(family: &str) -> bool {
    if is_builtin(family) {
        return bundled().is_some();
    }
    let fam = windows::core::HSTRING::from(family);
    with_collection(|coll| unsafe {
        let (mut index, mut exists) = (0u32, windows::core::BOOL(0));
        coll.FindFamilyName(&fam, &mut index, &mut exists).is_ok() && exists.as_bool()
    })
    .unwrap_or(false)
}

/// Every installed family as (English name, name in the user's language).
pub fn families() -> Vec<(String, String)> {
    use windows::Win32::Graphics::DirectWrite::IDWriteLocalizedStrings;
    use windows::Win32::Globalization::GetUserDefaultLocaleName;
    let mut buf = [0u16; 85];
    let n = unsafe { GetUserDefaultLocaleName(&mut buf) };
    let locale = windows::core::HSTRING::from_wide(&buf[..(n.max(1) as usize - 1)]);
    let name = |names: &IDWriteLocalizedStrings, loc: &windows::core::HSTRING| -> Option<String> {
        unsafe {
            let (mut i, mut found) = (0u32, windows::core::BOOL(0));
            names.FindLocaleName(loc, &mut i, &mut found).ok()?;
            if !found.as_bool() {
                return None;
            }
            let len = names.GetStringLength(i).ok()? as usize;
            let mut w = vec![0u16; len + 1];
            names.GetString(i, &mut w).ok()?;
            Some(String::from_utf16_lossy(&w[..len]))
        }
    };
    with_collection(|coll| unsafe {
        let mut out = Vec::new();
        for f in 0..coll.GetFontFamilyCount() {
            let Ok(fam) = coll.GetFontFamily(f) else { continue };
            let Ok(names) = fam.GetFamilyNames() else { continue };
            let en = name(&names, &windows::core::HSTRING::from("en-us")).or_else(|| {
                let len = names.GetStringLength(0).ok()? as usize;
                let mut w = vec![0u16; len + 1];
                names.GetString(0, &mut w).ok()?;
                Some(String::from_utf16_lossy(&w[..len]))
            });
            let Some(en) = en.filter(|s| !s.is_empty()) else { continue };
            let shown = name(&names, &locale).unwrap_or_else(|| en.clone());
            out.push((en, shown));
        }
        out
    })
    .unwrap_or_default()
}

/// The built-in font's collection, made once for the process.
struct Bundled(windows::Win32::Graphics::DirectWrite::IDWriteFontCollection);
// SAFETY: a font collection is immutable, and DirectWrite's objects made
// by the shared factory (all but text layouts and formats being changed)
// may be used from any thread; the COM reference count is atomic.
unsafe impl Send for Bundled {}
unsafe impl Sync for Bundled {}

static BUNDLED_COLLECTION: std::sync::OnceLock<Option<Bundled>> = std::sync::OnceLock::new();

#[cfg(test)]
thread_local! {
    /// Test: this thread sees the built-in font as unreadable (the path a
    /// PC whose DirectWrite cannot load it takes), whatever the process
    /// made of it.
    pub static FAIL_BUNDLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Pretendard from the exe's own bytes as a font collection of its own
/// (IDWriteFactory5's in-memory loader: Windows 10 1703 and later). None
/// when DirectWrite cannot read it: the UI font is then Windows' (pick).
/// The bytes are the exe's and never copied: the shared factory, which
/// lives as long as the process, stands as their owner.
pub fn bundled() -> Option<windows::Win32::Graphics::DirectWrite::IDWriteFontCollection> {
    #[cfg(test)]
    if FAIL_BUNDLED.with(|f| f.get()) {
        return None;
    }
    BUNDLED_COLLECTION
        .get_or_init(|| match load(BUNDLED_TTF) {
            Ok(c) => Some(Bundled(c)),
            Err(e) => {
                crate::log!("fonts: the built-in font cannot be read ({e}); drawing in Windows' font");
                None
            }
        })
        .as_ref()
        .map(|b| b.0.clone())
}

/// `bytes` (static: a reference is kept as long as text is drawn) as a
/// font collection holding BUNDLED_FAMILY. Err when DirectWrite cannot
/// read them or they hold no such family.
fn load(bytes: &'static [u8]) -> windows::core::Result<windows::Win32::Graphics::DirectWrite::IDWriteFontCollection> {
    use windows::core::Interface;
    use windows::Win32::Graphics::DirectWrite::{DWriteCreateFactory, IDWriteFactory5, IDWriteFontCollection, IDWriteFontSetBuilder1, DWRITE_FACTORY_TYPE_SHARED};
    unsafe {
        let dw: IDWriteFactory5 = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
        let loader = dw.CreateInMemoryFontFileLoader()?;
        // Registered for good: every reference made from it needs it for
        // as long as text is drawn.
        dw.RegisterFontFileLoader(&loader)?;
        let owner: windows::core::IUnknown = dw.cast()?;
        let file = loader.CreateInMemoryFontFileReference(&dw, bytes.as_ptr().cast(), bytes.len() as u32, &owner)?;
        let builder: IDWriteFontSetBuilder1 = dw.CreateFontSetBuilder()?.cast()?;
        // A variable font: its named instances (100 to 900) too.
        builder.AddFontFile(&file)?;
        let set = builder.CreateFontSet()?;
        let c: IDWriteFontCollection = dw.CreateFontCollectionFromFontSet(&set)?.cast()?;
        let (mut i, mut ok) = (0u32, windows::core::BOOL(0));
        let found = c.FindFamilyName(&windows::core::HSTRING::from(BUNDLED_FAMILY), &mut i, &mut ok).is_ok() && ok.as_bool();
        if !found {
            return Err(windows::core::Error::new(windows::Win32::Foundation::E_FAIL, format!("no family {BUNDLED_FAMILY:?}")));
        }
        Ok(c)
    }
}

/// `family` (as drawn: what `pick` returned) is the built-in Pretendard.
pub fn is_builtin(family: &str) -> bool {
    family.trim().eq_ignore_ascii_case(BUNDLED)
}

/// The name DirectWrite knows `family` by in its collection
/// (`collection_for`): BUNDLED_FAMILY for the built-in font, else itself.
pub fn dwrite_name(family: &str) -> &str {
    if is_builtin(family) {
        BUNDLED_FAMILY
    } else {
        family
    }
}

/// The collection a text format finds `family` in: the built-in one for
/// BUNDLED (by `dwrite_name`), else None (the system's: an installed
/// Pretendard too).
pub fn collection_for(family: &str) -> Option<windows::Win32::Graphics::DirectWrite::IDWriteFontCollection> {
    if is_builtin(family) {
        bundled()
    } else {
        None
    }
}

/// Pure: the line box (docs/design/harmony/font-results.md) for text in a
/// font whose vertical metrics are `m`, as (line height, baseline below the
/// line's top) in em. The height is Segoe UI's (ascent 2210 + descent 514
/// per 2048 em: the box every row, panel and paragraph was laid out in);
/// the baseline puts the font's optical middle (topbar::baseline::
/// Metrics::optical_mid) at the box's middle, so text centered by its box
/// is centered by its letters. (Segoe's own sits 0.05 em lower in its box:
/// kept at first, it read as text sagging in every control.) A font with
/// a shorter box of its own (Pretendard: 1.19 em against 1.33) keeps every
/// row's height.
pub fn segoe_line_box(m: &crate::topbar::baseline::Metrics) -> (f32, f32) {
    let segoe = crate::topbar::baseline::Metrics::SEGOE;
    const ASCENT: f32 = 2210.0;
    let line = (ASCENT + segoe.descent) / segoe.em;
    (line, line / 2.0 + m.optical_mid(1.0))
}

static BUNDLED_BOX: std::sync::OnceLock<(f32, f32)> = std::sync::OnceLock::new();

/// The built-in font's vertical metrics (Segoe's if they cannot be read).
pub fn bundled_metrics() -> crate::topbar::baseline::Metrics {
    use windows::Win32::Graphics::DirectWrite::{DWRITE_FONT_METRICS, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL};
    bundled()
        .and_then(|c| unsafe {
            let (mut i, mut ok) = (0u32, windows::core::BOOL(0));
            c.FindFamilyName(&windows::core::HSTRING::from(BUNDLED_FAMILY), &mut i, &mut ok).ok()?;
            let font = c.GetFontFamily(i).ok()?.GetFirstMatchingFont(DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL).ok()?;
            let mut fm = DWRITE_FONT_METRICS::default();
            font.GetMetrics(&mut fm);
            Some(crate::topbar::baseline::Metrics::of(fm.designUnitsPerEm, fm.descent, fm.capHeight))
        })
        .unwrap_or(crate::topbar::baseline::Metrics::SEGOE)
}

/// The built-in font's line box in em (segoe_line_box), read once.
pub fn bundled_line_box() -> (f32, f32) {
    *BUNDLED_BOX.get_or_init(|| segoe_line_box(&bundled_metrics()))
}

/// Pure: Pretendard's tracking (em, Latin and digits only) for text `size`
/// DIP (design system 4.1, font-results.md). It has no optical size: small
/// text keeps Caption's opening, body text none, and from Headline up it
/// tightens the way Inter (its Latin) is set: -1 % at 15, -1.5 % at 20,
/// -2 % from 26.
pub fn pretendard_tracking(size: f32) -> f32 {
    if size < 11.5 {
        0.005
    } else if size < 14.0 {
        0.0
    } else if size < 17.5 {
        -0.01
    } else if size < 23.0 {
        -0.015
    } else {
        -0.02
    }
}

/// The tracking (em) for ramp style `style` drawn in `family` (as mapped):
/// Pretendard's table for the built-in font, the ramp's own (made for
/// Segoe UI Variable's optical sizes) for every other family.
pub fn tracking(family: &str, style: &crate::theme::ramp::TextStyle) -> f32 {
    if is_builtin(family) {
        pretendard_tracking(style.size)
    } else {
        style.tracking
    }
}

#[cfg(test)]
#[path = "fonts_layout_tests.rs"]
mod layout_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_prefers_choice_then_theme_then_pretendard_then_system() {
        let all = |_: &str| true;
        assert_eq!(pick("Consolas", THEME_DEFAULT, "Segoe UI", all), "Consolas");
        assert_eq!(pick("", "Consolas", "Segoe UI", all), "Consolas", "a theme's own family");
        // The default: the built-in Pretendard, whatever Windows' font is.
        assert_eq!(pick("", THEME_DEFAULT, "Malgun Gothic", all), BUNDLED);
        assert_eq!(pick("", "", "", all), BUNDLED);
        // Windows' font, asked for: the message font; stock Segoe UI is
        // Segoe UI Variable when installed.
        assert_eq!(pick(SYSTEM_CHOICE, THEME_DEFAULT, "Malgun Gothic", all), "Malgun Gothic", "the user's Windows font");
        assert_eq!(pick(" System ", THEME_DEFAULT, "Segoe UI", all), THEME_DEFAULT);
        assert_eq!(pick(SYSTEM_CHOICE, "", "", all), THEME_DEFAULT);
        // The review (2026-11-11): Windows' font, asked for, wins over a
        // theme's own family (that is what the list says is drawn).
        assert_eq!(pick(SYSTEM_CHOICE, "Consolas", "Malgun Gothic", all), "Malgun Gothic");
        assert_eq!(pick(SYSTEM_CHOICE, "Consolas", "Segoe UI", all), THEME_DEFAULT);
        // ...and over the built-in font, readable or not.
        assert_eq!(pick(SYSTEM_CHOICE, THEME_DEFAULT, "Malgun Gothic", |f: &str| f != BUNDLED), "Malgun Gothic");
        // The review (2026-11-11): an installed Pretendard, static or
        // variable, is the user's own family (drawn from the PC); the
        // built-in one is only the default (""), never a name.
        assert_eq!(pick("Pretendard", "", "", all), "Pretendard");
        assert_eq!(pick("Pretendard Variable", "", "", all), "Pretendard Variable");
        assert_eq!(pick(" pretendard variable ", "", "", all), "pretendard variable");
        assert_ne!(pick("Pretendard Variable", "", "", all), BUNDLED);
        // One no longer installed: the default, the built-in one.
        let none_installed = |f: &str| !f.starts_with("Pretendard");
        assert_eq!(pick("Pretendard", "", "", none_installed), BUNDLED);
        assert_eq!(pick("Pretendard Variable", "", "", none_installed), BUNDLED);
        // The built-in font unreadable: Windows' font, as before.
        let no_bundled = |f: &str| f != BUNDLED;
        assert_eq!(pick("", THEME_DEFAULT, "Malgun Gothic", no_bundled), "Malgun Gothic");
        assert_eq!(pick("", THEME_DEFAULT, "Segoe UI", no_bundled), THEME_DEFAULT);
        assert_eq!(pick("Pretendard", "", "Segoe UI", |f: &str| f == THEME_DEFAULT), THEME_DEFAULT);
        let no_variable = |f: &str| f != THEME_DEFAULT && f != BUNDLED;
        assert_eq!(pick("", THEME_DEFAULT, "Segoe UI", no_variable), "Segoe UI");
        assert_eq!(pick(SYSTEM_CHOICE, THEME_DEFAULT, "", no_variable), "Segoe UI");
        // Not installed: passed over.
        let only_segoe = |f: &str| f == "Segoe UI";
        assert_eq!(pick("Gone Sans", "Old Theme Font", "Missing", only_segoe), "Segoe UI");
        assert_eq!(pick("Gone Sans", "", "", all), "Gone Sans");
        assert_eq!(pick("Gone Sans", "", "", |f: &str| f != "Gone Sans"), BUNDLED, "a choice not installed: the default");
        assert_eq!(pick("  Consolas  ", "", "", all), "Consolas", "trimmed");
    }

    #[test]
    fn the_built_in_font_reads() {
        // In the exe: drawn without being installed.
        assert!(installed(BUNDLED), "{BUNDLED}");
        assert!(collection_for(BUNDLED).is_some());
        assert!(collection_for("Segoe UI").is_none());
        // Its name is its own; DirectWrite finds it by its family in its
        // own collection. The family's name on the PC is the PC's.
        assert!(is_builtin(BUNDLED) && !is_builtin(BUNDLED_FAMILY) && !is_builtin("Pretendard"));
        assert_eq!((dwrite_name(BUNDLED), dwrite_name("Segoe UI"), dwrite_name(BUNDLED_FAMILY)), (BUNDLED_FAMILY, "Segoe UI", BUNDLED_FAMILY));
        assert!(collection_for(BUNDLED_FAMILY).is_none(), "an installed Pretendard Variable is the system's");
        // Its own metrics: a Latin cap height, a shorter box than Segoe's.
        let m = bundled_metrics();
        assert_ne!(m, crate::topbar::baseline::Metrics::SEGOE);
        assert!((0.68..0.76).contains(&(m.cap / m.em)), "{m:?}");
    }

    #[test]
    fn the_line_box_is_segoes() {
        use crate::topbar::baseline::Metrics;
        // Segoe's own: its ascent over descent, 1.33 em tall.
        let (line, base) = segoe_line_box(&Metrics::SEGOE);
        assert!((line - 2724.0 / 2048.0).abs() < 1e-5);
        // The optical middle at the box's middle.
        assert!((base - Metrics::SEGOE.optical_mid(1.0) - line / 2.0).abs() < 1e-5);
        // A font whose letters are taller sits lower by just as much of
        // its optical middle: the middle stays the box's.
        let tall = Metrics { em: 1000.0, descent: 250.0, cap: 740.0 };
        let (l2, b2) = segoe_line_box(&tall);
        assert_eq!(l2, line);
        assert!((b2 - tall.optical_mid(1.0) - line / 2.0).abs() < 1e-5);
        let (l3, b3) = bundled_line_box();
        assert_eq!(l3, line);
        assert!((1.0..1.2).contains(&b3), "{b3}");
    }

    #[test]
    fn tracking_follows_the_font() {
        use crate::theme::ramp;
        // Segoe UI Variable and every other family: the ramp's.
        for t in ramp::RAMP.iter().chain([&ramp::DISPLAY]) {
            assert_eq!(tracking(THEME_DEFAULT, t), t.tracking);
            assert_eq!(tracking("Malgun Gothic", t), t.tracking);
        }
        // Pretendard: open at 11, none for body, tighter as it grows.
        let p = |t: &ramp::TextStyle| tracking(BUNDLED, t);
        assert_eq!(p(&ramp::CAPTION), 0.005);
        assert_eq!((p(&ramp::FOOTNOTE), p(&ramp::BODY), p(&ramp::BODY_STRONG)), (0.0, 0.0, 0.0));
        assert_eq!((p(&ramp::HEADLINE), p(&ramp::TITLE), p(&ramp::LARGE_TITLE), p(&ramp::DISPLAY)), (-0.01, -0.015, -0.02, -0.02));
    }

    #[test]
    fn text_sizes_read_leniently() {
        assert_eq!(scale_of("small"), 0.92);
        assert_eq!(scale_of("LARGE"), 1.1);
        assert_eq!(scale_of("default"), 1.0);
        assert_eq!(scale_of(""), 1.0);
        assert_eq!(scale_of("huge"), 1.0);
        assert_eq!(size_index(" Small "), 0);
    }

    #[test]
    fn menu_lists_recommended_first_then_sorted() {
        let f = |en: &str, shown: &str| (en.to_string(), shown.to_string());
        let all = vec![f("Zeta", "Zeta"), f("Malgun Gothic", "맑은 고딕"), f("arial", "arial"), f("Segoe UI", "Segoe UI"), f("@Malgun Gothic", "@맑은 고딕"), f("Bodoni", "Bodoni"), f("Arial", "Arial")];
        let got: Vec<String> = menu_list(&all).into_iter().map(|(en, _)| en).collect();
        assert_eq!(got, ["Segoe UI", "Malgun Gothic", "arial", "Bodoni", "Zeta"]);
        // An installed Pretendard keeps its row (the review, 2026-11-11):
        // the built-in one is the list's default, above it.
        let with = vec![f("Pretendard", "Pretendard"), f("Pretendard Variable", "Pretendard Variable"), f("Pretendard JP", "Pretendard JP"), f("Segoe UI", "Segoe UI")];
        let got: Vec<String> = menu_list(&with).into_iter().map(|(en, _)| en).collect();
        assert_eq!(got, ["Segoe UI", "Pretendard", "Pretendard JP", "Pretendard Variable"]);
        assert!(menu_list(&[]).is_empty());
    }

    #[test]
    fn faces_follow_the_size() {
        use crate::theme::ramp::{self, Face};
        // Small up to 11, Text for 12 to 19, Display from 20 (spike S2).
        for (size, face) in [(10.0, Face::Small), (11.0, Face::Small), (12.0, Face::Text), (13.0, Face::Text), (15.0, Face::Text), (19.0, Face::Text), (20.0, Face::Display), (26.0, Face::Display), (48.0, Face::Display)] {
            assert_eq!(face_of(size), face, "{size}");
        }
        // The ramp's styles carry the same instance their size picks.
        for t in ramp::RAMP.iter().chain([&ramp::DISPLAY]) {
            assert_eq!(t.face, face_of(t.size), "{t:?}");
        }
        // Only Segoe UI Variable is swapped; any other family is kept.
        assert_eq!(face_for(THEME_DEFAULT, 11.0), "Segoe UI Variable Small");
        assert_eq!(face_for(THEME_DEFAULT, 13.0), "Segoe UI Variable Text");
        assert_eq!(face_for("segoe ui variable display", 13.0), "Segoe UI Variable Text");
        assert_eq!(face_for(THEME_DEFAULT, 26.0), "Segoe UI Variable Display");
        for other in ["Segoe UI", "Malgun Gothic", "Pretendard", BUNDLED, "Inter", ""] {
            assert_eq!(face_for(other, 26.0), other);
        }
    }

    #[test]
    fn map_keeps_other_families() {
        assert_eq!(map("Segoe UI Variable Display"), "Segoe UI Variable Display");
        assert_eq!(map("Consolas"), "Consolas");
    }
}
