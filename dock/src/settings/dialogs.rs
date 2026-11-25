//! The file open dialog, modal to the settings window.

use std::path::PathBuf;

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{FileOpenDialog, IFileOpenDialog, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_NODEREFERENCELINKS, SIGDN_FILESYSPATH};

fn specs(filters: &[(&str, &str)]) -> (Vec<HSTRING>, Vec<COMDLG_FILTERSPEC>) {
    let strings: Vec<HSTRING> = filters.iter().flat_map(|(n, p)| [HSTRING::from(*n), HSTRING::from(*p)]).collect();
    let specs = strings.chunks(2).map(|c| COMDLG_FILTERSPEC { pszName: PCWSTR(c[0].as_ptr()), pszSpec: PCWSTR(c[1].as_ptr()) }).collect();
    (strings, specs)
}

/// Asks for an existing file. `filters`: (name, "*.png;*.ico").
pub fn open(owner: HWND, title: &str, filters: &[(&str, &str)]) -> Option<PathBuf> {
    open_file(owner, title, filters, false)
}

/// open, where a picked shortcut (.lnk) is returned as itself, not as
/// what it points at (its arguments, folder and name are part of it).
pub fn open_keep_links(owner: HWND, title: &str, filters: &[(&str, &str)]) -> Option<PathBuf> {
    open_file(owner, title, filters, true)
}

fn open_file(owner: HWND, title: &str, filters: &[(&str, &str)], keep_links: bool) -> Option<PathBuf> {
    unsafe {
        let d: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let (_keep, specs) = specs(filters);
        d.SetFileTypes(&specs).ok()?;
        d.SetTitle(&HSTRING::from(title)).ok()?;
        let o = d.GetOptions().ok()?;
        let links = if keep_links { FOS_NODEREFERENCELINKS } else { Default::default() };
        d.SetOptions(o | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM | links).ok()?;
        d.Show(Some(owner)).ok()?;
        let item = d.GetResult().ok()?;
        let p = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.map(PathBuf::from)
    }
}

/// Windows' color dialog, modal to `owner`, open on its full form and
/// starting at `start`: the color picked (None: cancelled). Its 16 custom
/// slots last as long as the settings window's process.
pub fn pick_color(owner: HWND, start: crate::theme::Rgba) -> Option<crate::theme::Rgba> {
    use windows::Win32::Foundation::COLORREF;
    use windows::Win32::UI::Controls::Dialogs::{ChooseColorW, CC_FULLOPEN, CC_RGBINIT, CHOOSECOLORW};
    thread_local! {
        static SLOTS: std::cell::RefCell<[COLORREF; 16]> = const { std::cell::RefCell::new([COLORREF(0x00FF_FFFF); 16]) };
    }
    SLOTS.with(|slots| {
        let mut slots = slots.borrow_mut();
        let mut cc = CHOOSECOLORW {
            lStructSize: std::mem::size_of::<CHOOSECOLORW>() as u32,
            hwndOwner: owner,
            rgbResult: COLORREF(colorref(start)),
            lpCustColors: slots.as_mut_ptr(),
            Flags: CC_FULLOPEN | CC_RGBINIT,
            ..Default::default()
        };
        unsafe { ChooseColorW(&mut cc) }.as_bool().then(|| from_colorref(cc.rgbResult.0))
    })
}

/// 0x00BBGGRR, as GDI spells a color.
fn colorref(c: crate::theme::Rgba) -> u32 {
    c.r as u32 | (c.g as u32) << 8 | (c.b as u32) << 16
}

fn from_colorref(v: u32) -> crate::theme::Rgba {
    crate::theme::Rgba { r: (v & 0xFF) as u8, g: (v >> 8 & 0xFF) as u8, b: (v >> 16 & 0xFF) as u8, a: 255 }
}

#[cfg(test)]
mod tests {
    #[test]
    fn colorref_round_trips() {
        let c = crate::theme::Rgba { r: 0x12, g: 0x34, b: 0x56, a: 255 };
        assert_eq!(super::colorref(c), 0x0056_3412);
        assert_eq!(super::from_colorref(super::colorref(c)), c);
    }
}
