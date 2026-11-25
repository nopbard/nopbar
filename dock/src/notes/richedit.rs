//! The RichEdit (msftedit.dll, RICHEDIT50W) messages and structures the
//! notes use, from richedit.h (kept here rather than turning on another
//! `windows` crate feature). Structures are `#pragma pack(4)` in the
//! header; the ones holding pointers say so.

#![allow(dead_code)]

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, WM_USER};

pub const CLASS: PCWSTR = windows::core::w!("RICHEDIT50W");

pub const EM_SETSEL: u32 = 0x00B1;
pub const EM_LINEINDEX: u32 = 0x00BB;
pub const EM_LINELENGTH: u32 = 0x00C1;
pub const EM_REPLACESEL: u32 = 0x00C2;
pub const EM_CANUNDO: u32 = 0x00C6;
pub const EM_EMPTYUNDOBUFFER: u32 = 0x00CD;
pub const EM_POSFROMCHAR: u32 = 0x00D6;
pub const EM_CHARFROMPOS: u32 = 0x00D7;

pub const EM_EXGETSEL: u32 = WM_USER + 52;
pub const EM_EXLINEFROMCHAR: u32 = WM_USER + 54;
pub const EM_EXSETSEL: u32 = WM_USER + 55;
pub const EM_SETBKGNDCOLOR: u32 = WM_USER + 67;
pub const EM_SETCHARFORMAT: u32 = WM_USER + 68;
pub const EM_SETEVENTMASK: u32 = WM_USER + 69;
pub const EM_GETTEXTRANGE: u32 = WM_USER + 75;
pub const EM_SETUNDOLIMIT: u32 = WM_USER + 82;
pub const EM_SETTEXTMODE: u32 = WM_USER + 89;
pub const EM_GETTEXTEX: u32 = WM_USER + 94;
pub const EM_GETTEXTLENGTHEX: u32 = WM_USER + 95;
pub const EM_SETTEXTEX: u32 = WM_USER + 97;
pub const EM_GETSCROLLPOS: u32 = WM_USER + 221;

pub const ENM_CHANGE: u32 = 0x0000_0001;
pub const ENM_SCROLL: u32 = 0x0000_0004;
/// WM_COMMAND notifications (HIWORD of wParam).
pub const EN_CHANGE: u32 = 0x0300;
pub const EN_VSCROLL: u32 = 0x0602;

pub const TM_PLAINTEXT: u32 = 1;
pub const TM_RICHTEXT: u32 = 2;
pub const TM_MULTILEVELUNDO: u32 = 8;
pub const TM_MULTICODEPAGE: u32 = 32;

pub const SCF_DEFAULT: u32 = 0;
pub const SCF_SELECTION: u32 = 1;
pub const CFM_BOLD: u32 = 0x0000_0001;
pub const CFM_ITALIC: u32 = 0x0000_0002;
pub const CFM_UNDERLINE: u32 = 0x0000_0004;
pub const CFM_STRIKEOUT: u32 = 0x0000_0008;
pub const CFE_BOLD: u32 = 0x0000_0001;
pub const CFM_WEIGHT: u32 = 0x0040_0000;
pub const CFM_SPACING: u32 = 0x0020_0000;
pub const EM_GETCHARFORMAT: u32 = WM_USER + 58;
pub const EM_GETOLEINTERFACE: u32 = WM_USER + 60;
pub const EM_PASTESPECIAL: u32 = WM_USER + 64;
pub const EM_SETPARAFORMAT: u32 = WM_USER + 71;
pub const EM_GETPARAFORMAT: u32 = WM_USER + 61;
pub const EM_UNDO: u32 = 0x00C7;
pub const EM_SETLANGOPTIONS: u32 = WM_USER + 120;
pub const EM_GETLANGOPTIONS: u32 = WM_USER + 121;
pub const IMF_AUTOFONT: u32 = 0x0002;
pub const IMF_DUALFONT: u32 = 0x0080;
pub const EM_SETMARGINS: u32 = 0x00D3;
pub const EC_LEFTMARGIN: usize = 1;
pub const EC_RIGHTMARGIN: usize = 2;
pub const PFM_SPACEBEFORE: u32 = 0x0000_0040;
pub const PFM_SPACEAFTER: u32 = 0x0000_0080;
pub const PFM_LINESPACING: u32 = 0x0000_0100;
/// dyLineSpacing is the line's height exactly (twips).
pub const LINE_EXACT: u8 = 4;
/// Clipboard text, nothing else (CF_UNICODETEXT).
pub const CF_UNICODETEXT: usize = 13;
pub const SCF_ALL: u32 = 4;
pub const CFM_SIZE: u32 = 0x8000_0000;
pub const CFM_COLOR: u32 = 0x4000_0000;
pub const CFM_FACE: u32 = 0x2000_0000;

/// UTF-16.
pub const CP_UNICODE: u32 = 1200;
pub const GT_DEFAULT: u32 = 0;
pub const GTL_PRECISE: u32 = 2;
pub const GTL_NUMCHARS: u32 = 8;
pub const ST_DEFAULT: u32 = 0;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct CharRange {
    pub min: i32,
    pub max: i32,
}

/// CHARFORMAT2W (116 bytes; no member needs more than 4-byte alignment).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CharFormat2 {
    pub cb_size: u32,
    pub mask: u32,
    pub effects: u32,
    pub height: i32,
    pub offset: i32,
    pub color: u32,
    pub charset: u8,
    pub pitch_and_family: u8,
    pub face: [u16; 32],
    pub weight: u16,
    pub spacing: i16,
    pub back_color: u32,
    pub lcid: u32,
    pub reserved: u32,
    pub style: i16,
    pub kerning: u16,
    pub underline_type: u8,
    pub animation: u8,
    pub rev_author: u8,
    pub underline_color: u8,
}

impl Default for CharFormat2 {
    fn default() -> Self {
        // SAFETY: plain data; all zero is a valid (empty) CHARFORMAT2W.
        let mut c: CharFormat2 = unsafe { std::mem::zeroed() };
        c.cb_size = std::mem::size_of::<CharFormat2>() as u32;
        c
    }
}

/// PARAFORMAT2 (188 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ParaFormat2 {
    pub cb_size: u32,
    pub mask: u32,
    pub numbering: u16,
    pub effects: u16,
    pub start_indent: i32,
    pub right_indent: i32,
    pub offset: i32,
    pub alignment: u16,
    pub tab_count: i16,
    pub tabs: [i32; 32],
    pub space_before: i32,
    pub space_after: i32,
    pub line_spacing: i32,
    pub style: i16,
    pub line_spacing_rule: u8,
    pub outline_level: u8,
    pub shading_weight: u16,
    pub shading_style: u16,
    pub numbering_start: u16,
    pub numbering_style: u16,
    pub numbering_tab: u16,
    pub border_space: u16,
    pub border_width: u16,
    pub borders: u16,
}

impl Default for ParaFormat2 {
    fn default() -> Self {
        // SAFETY: plain data; all zero is a valid (empty) PARAFORMAT2.
        let mut p: ParaFormat2 = unsafe { std::mem::zeroed() };
        p.cb_size = std::mem::size_of::<ParaFormat2>() as u32;
        p
    }
}

/// The text object model of a RichEdit (ITextDocument, through
/// EM_GETOLEINTERFACE), for the two calls the notes need: Freeze /
/// Unfreeze (one repaint for many changes) and Undo(tomSuspend /
/// tomResume) (changes kept out of the undo history). Called through its
/// vtable: the `windows` crate's RichEdit feature is not turned on.
pub struct Tom(*mut std::ffi::c_void);

/// ITextDocument's IID.
const IID_ITEXTDOCUMENT: windows::core::GUID = windows::core::GUID::from_u128(0x8CC497C0_A1DF_11CE_8098_00AA0047BE5D);
const TOM_SUSPEND: i32 = -9_999_995;
const TOM_RESUME: i32 = -9_999_994;

impl Tom {
    pub fn of(h: impl Target) -> Option<Tom> {
        use windows::core::Interface;
        let mut ole: *mut std::ffi::c_void = std::ptr::null_mut();
        send(h, EM_GETOLEINTERFACE, 0, &mut ole as *mut _ as isize);
        if ole.is_null() {
            return None;
        }
        // SAFETY: EM_GETOLEINTERFACE hands out an AddRef'd IRichEditOle.
        let unk = unsafe { windows::core::IUnknown::from_raw(ole) };
        let mut doc: *mut std::ffi::c_void = std::ptr::null_mut();
        unsafe { unk.query(&IID_ITEXTDOCUMENT, &mut doc) }.ok().ok()?;
        (!doc.is_null()).then_some(Tom(doc))
    }
    /// SAFETY: `idx` is an ITextDocument method taking (long, long*).
    unsafe fn long_call(&self, idx: usize, arg: i32) {
        let vt = *(self.0 as *const *const usize);
        let f: extern "system" fn(*mut std::ffi::c_void, i32, *mut i32) -> windows::core::HRESULT = std::mem::transmute(*vt.add(idx));
        let mut out = 0;
        let _ = f(self.0, arg, &mut out);
    }
    /// SAFETY: `idx` is an ITextDocument method taking (long*).
    unsafe fn out_call(&self, idx: usize) {
        let vt = *(self.0 as *const *const usize);
        let f: extern "system" fn(*mut std::ffi::c_void, *mut i32) -> windows::core::HRESULT = std::mem::transmute(*vt.add(idx));
        let mut out = 0;
        let _ = f(self.0, &mut out);
    }
    /// Runs `f` with nothing drawn and nothing recorded for Undo, then
    /// draws once.
    pub fn quietly<R>(&self, f: impl FnOnce() -> R) -> R {
        unsafe {
            self.out_call(18); // Freeze
            self.long_call(22, TOM_SUSPEND); // Undo(tomSuspend)
        }
        let r = f();
        unsafe {
            self.long_call(22, TOM_RESUME);
            self.out_call(19); // Unfreeze
        }
        r
    }
}

impl Drop for Tom {
    fn drop(&mut self) {
        // SAFETY: the reference query() gave us.
        drop(unsafe { <windows::core::IUnknown as windows::core::Interface>::from_raw(self.0) });
    }
}

/// TEXTRANGEW.
#[repr(C, packed(4))]
pub struct TextRange {
    pub chrg: CharRange,
    pub text: PWSTR,
}

/// GETTEXTEX.
#[repr(C, packed(4))]
pub struct GetTextEx {
    pub cb: u32,
    pub flags: u32,
    pub codepage: u32,
    pub default_char: *const u8,
    pub used_default: *mut i32,
}

/// GETTEXTLENGTHEX / SETTEXTEX (two DWORDs each).
#[repr(C)]
pub struct TwoFlags {
    pub flags: u32,
    pub codepage: u32,
}

/// What RichEdit messages go to: a RichEdit window, or a windowless one
/// (crate::notes::textbox) through its text services.
pub trait Target {
    fn send(&self, msg: u32, wp: usize, lp: isize) -> isize;
}

impl Target for HWND {
    fn send(&self, msg: u32, wp: usize, lp: isize) -> isize {
        unsafe { SendMessageW(*self, msg, Some(WPARAM(wp)), Some(LPARAM(lp))) }.0
    }
}

impl<T: Target + ?Sized> Target for &T {
    fn send(&self, msg: u32, wp: usize, lp: isize) -> isize {
        (**self).send(msg, wp, lp)
    }
}

pub fn send(h: impl Target, msg: u32, wp: usize, lp: isize) -> isize {
    h.send(msg, wp, lp)
}

/// The selection (UTF-16 positions).
pub fn sel(h: impl Target + Copy) -> (i32, i32) {
    let mut r = CharRange::default();
    send(h, EM_EXGETSEL, 0, &mut r as *mut _ as isize);
    (r.min, r.max)
}

pub fn set_sel(h: impl Target + Copy, a: i32, b: i32) {
    let r = CharRange { min: a, max: b };
    send(h, EM_EXSETSEL, 0, &r as *const _ as isize);
}

pub fn line_of(h: impl Target + Copy, cp: i32) -> i32 {
    send(h, EM_EXLINEFROMCHAR, 0, cp as isize) as i32
}

pub fn line_start(h: impl Target + Copy, line: i32) -> i32 {
    send(h, EM_LINEINDEX, line as usize, 0) as i32
}

/// Text in [a, b) (UTF-16 positions).
pub fn range(h: impl Target + Copy, a: i32, b: i32) -> Vec<u16> {
    if b <= a {
        return Vec::new();
    }
    let mut buf = vec![0u16; (b - a) as usize + 1];
    let mut tr = TextRange { chrg: CharRange { min: a, max: b }, text: PWSTR(buf.as_mut_ptr()) };
    let n = send(h, EM_GETTEXTRANGE, 0, &mut tr as *mut _ as isize).max(0) as usize;
    buf.truncate(n.min(buf.len()));
    buf
}

/// Line `line`'s text without its break, and where it starts.
pub fn line(h: impl Target + Copy, line: i32) -> (i32, String) {
    let start = line_start(h, line);
    if start < 0 {
        return (0, String::new());
    }
    let len = send(h, EM_LINELENGTH, start as usize, 0) as i32;
    (start, String::from_utf16_lossy(&range(h, start, start + len)))
}

/// Replaces [a, b) with `text` as one undoable step.
pub fn replace(h: impl Target + Copy, a: i32, b: i32, text: &str) {
    set_sel(h, a, b);
    let w: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    send(h, EM_REPLACESEL, 1, w.as_ptr() as isize);
}

/// The whole text (line breaks as RichEdit keeps them: "\r").
pub fn text(h: impl Target + Copy) -> String {
    let len = TwoFlags { flags: GTL_NUMCHARS | GTL_PRECISE, codepage: CP_UNICODE };
    let n = send(h, EM_GETTEXTLENGTHEX, &len as *const _ as usize, 0).max(0) as usize;
    let mut buf = vec![0u16; n + 1];
    let gt = GetTextEx { cb: (buf.len() * 2) as u32, flags: GT_DEFAULT, codepage: CP_UNICODE, default_char: std::ptr::null(), used_default: std::ptr::null_mut() };
    let got = send(h, EM_GETTEXTEX, &gt as *const _ as usize, buf.as_mut_ptr() as isize).max(0) as usize;
    String::from_utf16_lossy(&buf[..got.min(n)])
}

/// Replaces the whole text (and forgets Undo).
pub fn set_text(h: impl Target + Copy, s: &str) {
    let st = TwoFlags { flags: ST_DEFAULT, codepage: CP_UNICODE };
    let w: Vec<u16> = s.replace('\n', "\r").encode_utf16().chain(std::iter::once(0)).collect();
    send(h, EM_SETTEXTEX, &st as *const _ as usize, w.as_ptr() as isize);
    send(h, EM_EMPTYUNDOBUFFER, 0, 0);
}

/// Where character `cp` is drawn (client px; its top left).
pub fn pos_of(h: impl Target + Copy, cp: i32) -> POINT {
    let mut p = POINT::default();
    send(h, EM_POSFROMCHAR, &mut p as *mut _ as usize, cp as isize);
    p
}

/// The character nearest client point `p`.
pub fn char_at(h: impl Target + Copy, p: POINT) -> i32 {
    send(h, EM_CHARFROMPOS, 0, &p as *const _ as isize) as i32
}

pub fn scroll_pos(h: impl Target + Copy) -> POINT {
    let mut p = POINT::default();
    send(h, EM_GETSCROLLPOS, 0, &mut p as *mut _ as isize);
    p
}

pub fn len(h: impl Target + Copy) -> i32 {
    let l = TwoFlags { flags: GTL_NUMCHARS | GTL_PRECISE, codepage: CP_UNICODE };
    send(h, EM_GETTEXTLENGTHEX, &l as *const _ as usize, 0) as i32
}

/// Pure: a font of `size` DIP on a window at `dpi`, in twips for a
/// RichEdit that turns twips into pixels with the screen's `logpy`.
pub fn twips(size: f32, dpi: u32, logpy: i32) -> i32 {
    let px = size * dpi.max(96) as f32 / 96.0;
    (px * 1440.0 / logpy.max(1) as f32).round() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structures_match_the_header() {
        assert_eq!(std::mem::size_of::<CharFormat2>(), 116);
        assert_eq!(std::mem::size_of::<ParaFormat2>(), 188);
        assert_eq!(std::mem::size_of::<GetTextEx>(), if cfg!(target_pointer_width = "64") { 28 } else { 20 });
        assert_eq!(std::mem::size_of::<TextRange>(), if cfg!(target_pointer_width = "64") { 16 } else { 12 });
        assert_eq!(std::mem::size_of::<CharRange>(), 8);
    }

    #[test]
    fn font_sizes_follow_the_display() {
        // 13 DIP at 100 %: 9.75 pt.
        assert_eq!(twips(13.0, 96, 96), 195);
        // Same DPI as the screen's: the same points.
        assert_eq!(twips(13.0, 144, 144), 195);
        // A 150 % display while the screen DC says 100 %: bigger.
        assert_eq!(twips(13.0, 144, 96), 293);
    }
}
