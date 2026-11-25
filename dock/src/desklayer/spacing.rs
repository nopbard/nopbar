//! Windows' icon spacing when Explorer has not answered yet. No guessed grid.
use super::grid::Desk;
use windows::{
    core::{w, PCWSTR},
    Win32::{
        System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ},
        UI::{
            HiDpi::GetSystemMetricsForDpi,
            WindowsAndMessaging::{SM_CXICONSPACING, SM_CYICONSPACING},
        },
    },
};

/// Negative WindowMetrics values are twips; positive values are 96-DPI px.
pub fn pixels(value: i32, dpi: u32) -> Option<i32> {
    if value == 0 {
        return None;
    }
    let denominator = if value < 0 { 1440 } else { 96 };
    let n = (i64::from(value).abs() * i64::from(dpi) + denominator / 2) / denominator;
    i32::try_from(n).ok().filter(|v| *v > 0)
}
fn metric(name: PCWSTR, dpi: u32) -> Option<i32> {
    let mut text = [0u16; 64];
    let mut bytes = std::mem::size_of_val(&text) as u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Control Panel\\Desktop\\WindowMetrics"),
            name,
            RRF_RT_REG_SZ,
            None,
            Some(text.as_mut_ptr() as _),
            Some(&mut bytes),
        )
        .ok()
        .ok()?;
    }
    let n = text.iter().position(|v| *v == 0).unwrap_or(text.len());
    pixels(
        String::from_utf16_lossy(&text[..n]).trim().parse().ok()?,
        dpi,
    )
}
pub fn fallback(scale: f32) -> Desk {
    let dpi = (scale * 96.).round().max(96.) as u32;
    let cell = unsafe {
        (
            metric(w!("IconSpacing"), dpi)
                .unwrap_or_else(|| GetSystemMetricsForDpi(SM_CXICONSPACING, dpi)),
            metric(w!("IconVerticalSpacing"), dpi)
                .unwrap_or_else(|| GetSystemMetricsForDpi(SM_CYICONSPACING, dpi)),
        )
    };
    Desk {
        cell,
        spacing_dpi: dpi,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registry_twips_and_pixel_values_follow_dpi() {
        assert_eq!(pixels(-1140, 96), Some(76));
        assert_eq!(pixels(-1440, 144), Some(144));
        assert_eq!(pixels(-1200, 144), Some(120));
        assert_eq!(pixels(80, 144), Some(120));
        assert_eq!(pixels(0, 96), None);
    }
}
