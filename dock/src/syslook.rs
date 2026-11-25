//! What Windows says about appearance: light/dark, accent color (the "on"
//! color of the light palette by default; dark stays achromatic), transparency
//! effects, high contrast, animations, the UI font. Read on demand (startup
//! and WM_SETTINGCHANGE & co.), never polled.

use windows::core::{w, PCWSTR};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows::Win32::Graphics::Gdi::{GetSysColor, COLOR_GRAYTEXT, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT};
use windows::Win32::UI::WindowsAndMessaging::{
    SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION, SPI_GETHIGHCONTRAST, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};

use crate::theme::Rgba;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SysLook {
    /// Windows mode (taskbar/Start) is light.
    pub light: bool,
    pub accent: Rgba,
    /// "Transparency effects" is on.
    pub transparency: bool,
    /// High contrast colors (window, text, highlight) when on.
    pub high_contrast: Option<HighContrast>,
    /// "Animation effects" is on.
    pub animations: bool,
    /// A hash of Windows' UI font (crate::fonts): a new one re-themes.
    pub ui_font: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HighContrast {
    pub window: Rgba,
    pub text: Rgba,
    pub highlight: Rgba,
    pub highlight_text: Rgba,
    /// GrayText: what cannot be used (Windows' contrast themes' "Disabled
    /// text"), so a disabled row is told from the rest (states-issues).
    pub gray_text: Rgba,
}

/// GrayText of Windows' own contrast themes (Aquatic, Dusk, Night sky):
/// the stand-in where a test or a picture names the other four colors.
#[cfg(test)]
pub const GRAY_TEXT: Rgba = Rgba { r: 0xA6, g: 0xA6, b: 0xA6, a: 255 };

impl Default for SysLook {
    fn default() -> Self {
        Self {
            light: false,
            accent: Rgba { r: 0x00, g: 0x78, b: 0xD4, a: 255 },
            transparency: true,
            high_contrast: None,
            animations: true,
            ui_font: 0,
        }
    }
}

/// A DWORD under HKCU (None if missing or not a DWORD).
pub(crate) fn dword(key: PCWSTR, value: PCWSTR) -> Option<u32> {
    let mut data = 0u32;
    let mut size = 4u32;
    unsafe {
        RegGetValueW(HKEY_CURRENT_USER, key, value, RRF_RT_REG_DWORD, None, Some(&mut data as *mut u32 as *mut _), Some(&mut size))
            .ok()
            .ok()?;
    }
    Some(data)
}

/// 0x00BBGGRR (COLORREF / GetSysColor) -> Rgba.
pub fn from_colorref(c: u32) -> Rgba {
    Rgba { r: (c & 0xFF) as u8, g: ((c >> 8) & 0xFF) as u8, b: ((c >> 16) & 0xFF) as u8, a: 255 }
}

pub fn current() -> SysLook {
    let personalize = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
    let light = dword(personalize, w!("SystemUsesLightTheme")).is_some_and(|v| v != 0);
    let transparency = dword(personalize, w!("EnableTransparency")).is_none_or(|v| v != 0);
    // DWM AccentColor is 0xAABBGGRR.
    let accent = dword(w!("Software\\Microsoft\\Windows\\DWM"), w!("AccentColor"))
        .map(from_colorref)
        .unwrap_or(SysLook::default().accent);
    let high_contrast = unsafe {
        let mut hc = HIGHCONTRASTW { cbSize: std::mem::size_of::<HIGHCONTRASTW>() as u32, ..Default::default() };
        let ok = SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            hc.cbSize,
            Some(&mut hc as *mut _ as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .is_ok();
        (ok && hc.dwFlags.contains(HCF_HIGHCONTRASTON)).then(|| HighContrast {
            window: from_colorref(GetSysColor(COLOR_WINDOW)),
            text: from_colorref(GetSysColor(COLOR_WINDOWTEXT)),
            highlight: from_colorref(GetSysColor(COLOR_HIGHLIGHT)),
            highlight_text: from_colorref(GetSysColor(COLOR_HIGHLIGHTTEXT)),
            gray_text: from_colorref(GetSysColor(COLOR_GRAYTEXT)),
        })
    };
    let animations = unsafe {
        let mut on = windows::core::BOOL(1);
        let _ = SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some(&mut on as *mut _ as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
        on.as_bool()
    };
    let ui_font = crate::fonts::refresh_system();
    SysLook { light, accent, transparency, high_contrast, animations, ui_font }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colorref_order() {
        assert_eq!(from_colorref(0x00D47800), Rgba { r: 0x00, g: 0x78, b: 0xD4, a: 255 });
    }

    #[test]
    fn reading_does_not_panic() {
        let _ = current();
    }
}
