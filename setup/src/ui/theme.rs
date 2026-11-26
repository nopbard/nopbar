//! Colors: near-white or near-black, achromatic, following the Windows app
//! mode. High contrast takes the system colors and draws outlines.

use windows::core::w;
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::Gdi::{GetSysColor, COLOR_HIGHLIGHT, COLOR_WINDOW, COLOR_WINDOWTEXT};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION, SPI_GETHIGHCONTRAST, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Rgba {
    pub const fn hex(v: u32) -> Rgba {
        Rgba { r: ((v >> 16) & 0xFF) as f32 / 255.0, g: ((v >> 8) & 0xFF) as f32 / 255.0, b: (v & 0xFF) as f32 / 255.0, a: 1.0 }
    }
    /// 0x00BBGGRR (GetSysColor).
    pub fn colorref(c: u32) -> Rgba {
        Rgba::hex(((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF))
    }
    pub fn alpha(self, a: f32) -> Rgba {
        Rgba { a, ..self }
    }
    /// `t` of self over `under` (CSS color-mix(in srgb, self t, under)).
    pub fn mix(self, under: Rgba, t: f32) -> Rgba {
        let m = |a: f32, b: f32| a * t + b * (1.0 - t);
        Rgba { r: m(self.r, under.r), g: m(self.g, under.g), b: m(self.b, under.b), a: 1.0 }
    }
    pub fn d2d(self) -> D2D1_COLOR_F {
        D2D1_COLOR_F { r: self.r, g: self.g, b: self.b, a: self.a }
    }
}

/// Windows' high contrast colors.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HighContrast {
    pub window: Rgba,
    pub text: Rgba,
    pub highlight: Rgba,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub dark: bool,
    pub bg: Rgba,
    pub strip: Rgba,
    pub ink: Rgba,
    /// Secondary: the hint, the arrow, the close glyph.
    pub ink2: Rgba,
    pub track: Rgba,
    pub fill: Rgba,
    pub folder_back: Rgba,
    pub folder_front: Rgba,
    pub folder_glyph: Rgba,
    pub hover: Rgba,
    pub focus: Rgba,
    /// High contrast: shapes get a 1 px outline in this color.
    pub outline: Option<Rgba>,
}

pub fn palette(dark: bool, hc: Option<HighContrast>) -> Palette {
    if let Some(hc) = hc {
        return Palette {
            dark,
            bg: hc.window,
            strip: hc.window,
            ink: hc.text,
            ink2: hc.text,
            track: hc.window,
            fill: hc.highlight,
            folder_back: hc.window,
            folder_front: hc.window,
            folder_glyph: hc.text,
            hover: hc.highlight,
            focus: hc.highlight,
            outline: Some(hc.text),
        };
    }
    let (bg, strip, ink) = if dark {
        (Rgba::hex(0x1E1E1F), Rgba::hex(0x262627), Rgba::hex(0xF2F2F2))
    } else {
        (Rgba::hex(0xFFFFFF), Rgba::hex(0xF6F6F6), Rgba::hex(0x1D1D1F))
    };
    Palette {
        dark,
        bg,
        strip,
        ink,
        ink2: ink.alpha(if dark { 0.55 } else { 0.5 }),
        track: if dark { Rgba::hex(0xFFFFFF).alpha(0.12) } else { Rgba::hex(0x000000).alpha(0.08) },
        fill: ink,
        folder_back: ink.mix(bg, 0.22),
        folder_front: ink.mix(bg, 0.34),
        folder_glyph: bg,
        hover: ink.alpha(if dark { 0.10 } else { 0.07 }),
        focus: ink.alpha(0.85),
        outline: None,
    }
}

/// What Windows says about appearance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    /// Apps use dark mode (Settings > Personalization > Colors).
    pub dark: bool,
    pub high_contrast: Option<HighContrast>,
    /// "Animation effects" is on.
    pub animations: bool,
}

pub fn current() -> Look {
    let mut light = 1u32;
    let mut size = 4u32;
    let read = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut light as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    let dark = read.is_ok() && light == 0;
    let high_contrast = unsafe {
        let mut hc = HIGHCONTRASTW { cbSize: std::mem::size_of::<HIGHCONTRASTW>() as u32, ..Default::default() };
        let ok = SystemParametersInfoW(SPI_GETHIGHCONTRAST, hc.cbSize, Some(&mut hc as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)).is_ok();
        (ok && hc.dwFlags.contains(HCF_HIGHCONTRASTON)).then(|| HighContrast {
            window: Rgba::colorref(GetSysColor(COLOR_WINDOW)),
            text: Rgba::colorref(GetSysColor(COLOR_WINDOWTEXT)),
            highlight: Rgba::colorref(GetSysColor(COLOR_HIGHLIGHT)),
        })
    };
    let animations = unsafe {
        let mut on = windows::core::BOOL(1);
        let _ = SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, Some(&mut on as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
        on.as_bool()
    };
    Look { dark, high_contrast, animations }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn achromatic(c: Rgba) -> bool {
        (c.r - c.g).abs() < 0.01 && (c.g - c.b).abs() < 0.01
    }

    #[test]
    fn both_modes_are_achromatic_with_no_outlines() {
        for dark in [false, true] {
            let p = palette(dark, None);
            for c in [p.bg, p.strip, p.ink, p.ink2, p.track, p.fill, p.folder_back, p.folder_front, p.folder_glyph, p.hover, p.focus] {
                assert!(achromatic(c), "{c:?}");
            }
            assert_eq!(p.outline, None);
            // Readable: the ink far from the background.
            let lum = |c: Rgba| 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
            assert!((lum(p.ink) - lum(p.bg)).abs() > 0.8);
            assert_eq!(dark, lum(p.bg) < 0.5);
            // The folder sits between the background and the ink.
            assert!((lum(p.folder_back) - lum(p.bg)).abs() < (lum(p.folder_front) - lum(p.bg)).abs());
        }
    }

    #[test]
    fn high_contrast_uses_system_colors_and_outlines() {
        let hc = HighContrast { window: Rgba::hex(0x000000), text: Rgba::hex(0xFFFF00), highlight: Rgba::hex(0x1AEBFF) };
        let p = palette(false, Some(hc));
        assert_eq!(p.bg, hc.window);
        assert_eq!(p.ink, hc.text);
        assert_eq!(p.ink2, hc.text, "no dimmed text in high contrast");
        assert_eq!(p.fill, hc.highlight);
        assert_eq!(p.outline, Some(hc.text));
    }

    #[test]
    fn color_helpers() {
        assert_eq!(Rgba::colorref(0x00D47800), Rgba::hex(0x0078D4));
        let m = Rgba::hex(0x000000).mix(Rgba::hex(0xFFFFFF), 0.25);
        assert!((m.r - 0.75).abs() < 1e-6);
        let _ = current();
    }
}
