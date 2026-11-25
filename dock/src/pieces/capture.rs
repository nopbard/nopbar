//! Taking the screen's picture for a snip (snip.rs): one still copy of a
//! monitor, made before the overlay exists (so the overlay is never in
//! it). GDI's BitBlt of the screen: synchronous, no capture border, no
//! WinRT session; with DWM it copies what is composed on screen, and
//! windows kept out of captures (WDA_EXCLUDEFROMCAPTURE: our widgets while
//! Settings has them hidden from screen sharing) are left out of it. Tests give their own capturer.

use windows::Win32::Foundation::RECT;

use super::snip::Frame;

/// Something that takes the screen's picture.
pub trait Capturer {
    /// The picture of screen px `r`, or None.
    fn grab(&self, r: RECT) -> Option<Frame>;
}

/// The real screen.
pub struct Screen;

impl Capturer for Screen {
    fn grab(&self, r: RECT) -> Option<Frame> {
        use windows::Win32::Graphics::Gdi::{
            BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush, GetDC, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER,
            BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, SRCCOPY,
        };
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        if w <= 0 || h <= 0 {
            return None;
        }
        let len = crate::gfx::bgra_len(w as u32, h as u32)?;
        unsafe {
            let screen = GetDC(None);
            if screen.is_invalid() {
                return None;
            }
            let mem = CreateCompatibleDC(Some(screen));
            let bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    // Top-down rows.
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            let out = match CreateDIBSection(Some(screen), &bi, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(dib) if !bits.is_null() => {
                    let old = SelectObject(mem, dib.into());
                    let copied = BitBlt(mem, 0, 0, w, h, Some(screen), r.left, r.top, SRCCOPY | CAPTUREBLT).is_ok();
                    let _ = GdiFlush();
                    let frame = copied.then(|| {
                        let mut px = std::slice::from_raw_parts(bits as *const u8, len).to_vec();
                        // GDI leaves the fourth byte undefined: the screen is opaque.
                        for p in px.as_chunks_mut::<4>().0 {
                            p[3] = 255;
                        }
                        Frame { w: w as u32, h: h as u32, px }
                    });
                    SelectObject(mem, old);
                    let _ = DeleteObject(dib.into());
                    frame
                }
                _ => None,
            };
            let _ = DeleteDC(mem);
            ReleaseDC(None, screen);
            out
        }
    }
}

#[cfg(test)]
pub mod fake {
    //! A capturer that never touches the screen.

    use super::*;

    /// Paints a known picture of any size: B = x, G = y, R = 99.
    pub struct Painted {
        pub asked: std::cell::RefCell<Vec<RECT>>,
    }

    impl Capturer for Painted {
        fn grab(&self, r: RECT) -> Option<Frame> {
            self.asked.borrow_mut().push(r);
            let (w, h) = ((r.right - r.left).max(0) as u32, (r.bottom - r.top).max(0) as u32);
            if w == 0 || h == 0 {
                return None;
            }
            let mut px = Vec::with_capacity((w * h * 4) as usize);
            for y in 0..h {
                for x in 0..w {
                    px.extend_from_slice(&[x as u8, y as u8, 99, 255]);
                }
            }
            Some(Frame { w, h, px })
        }
    }
}
