//! The clipboard's picture and the image codecs (WIC), for the screenshot
//! worker. Nothing here logs what a picture shows.

use std::sync::Arc;

use windows::core::{w, Result, GUID};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HGLOBAL, HWND};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppBGRA, GUID_WICPixelFormat32bppPBGRA, IWICBitmapFrameDecode,
    IWICBitmapFrameEncode, IWICImagingFactory, WICBitmapDitherTypeNone, WICBitmapEncoderNoCache, WICBitmapInterpolationModeFant,
    WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{CoCreateInstance, IStream, CLSCTX_INPROC_SERVER, STATFLAG_NONAME, STATSTG, STREAM_SEEK_SET};
use windows::Win32::System::DataExchange::{
    CloseClipboard, GetClipboardData, GetClipboardOwner, GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW,
};
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

pub const CF_DIB: u32 = 8;
pub const CF_DIBV5: u32 = 17;

/// The registered "PNG" clipboard format.
pub fn cf_png() -> u32 {
    unsafe { RegisterClipboardFormatW(w!("PNG")) }
}

/// What the clipboard holds now, without opening it: a picture? from a
/// capture tool?
pub fn look() -> super::Look {
    unsafe {
        let image = [cf_png(), CF_DIB, CF_DIBV5].iter().any(|f| IsClipboardFormatAvailable(*f).is_ok());
        let owner = GetClipboardOwner().ok().filter(|h| !h.is_invalid());
        let exe = owner.and_then(|h| exe_of(h));
        // An owner whose program cannot be read is not taken for a tool.
        let capture = match owner {
            None => super::is_capture_tool(None),
            Some(_) => exe.as_deref().is_some_and(|e| super::is_capture_tool(Some(e))),
        };
        super::Look { image, capture }
    }
}

unsafe fn exe_of(hwnd: HWND) -> Option<String> {
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid == 0 {
        return None;
    }
    let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
    let _ = CloseHandle(h);
    ok.then(|| String::from_utf16_lossy(&buf[..len as usize]))
}

/// A picture read off the clipboard: the encoded bytes and their kind.
pub enum Raw {
    Png(Vec<u8>),
    Dib(Vec<u8>),
}

/// Copies the clipboard's picture (PNG first, else a DIB) if the clipboard
/// is still at `seq`. Retries a moment while another app holds it open.
pub fn read(seq: u32) -> Option<Raw> {
    unsafe {
        let mut opened = false;
        for _ in 0..10 {
            if OpenClipboard(None).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        if !opened {
            return None;
        }
        let r = (|| {
            if GetClipboardSequenceNumber() != seq {
                return None;
            }
            for (fmt, png) in [(cf_png(), true), (CF_DIB, false), (CF_DIBV5, false)] {
                if IsClipboardFormatAvailable(fmt).is_err() {
                    continue;
                }
                let Ok(h) = GetClipboardData(fmt) else { continue };
                if let Some(bytes) = global_bytes(h) {
                    return Some(if png { Raw::Png(bytes) } else { Raw::Dib(bytes) });
                }
            }
            None
        })();
        let _ = CloseClipboard();
        r
    }
}

unsafe fn global_bytes(h: HANDLE) -> Option<Vec<u8>> {
    let g = HGLOBAL(h.0);
    let n = GlobalSize(g);
    if n == 0 || n > super::MAX_BYTES {
        return None;
    }
    let p = GlobalLock(g) as *const u8;
    if p.is_null() {
        return None;
    }
    let v = std::slice::from_raw_parts(p, n).to_vec();
    let _ = GlobalUnlock(g);
    Some(v)
}

fn factory() -> Result<IWICImagingFactory> {
    unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
}

fn decode(f: &IWICImagingFactory, bytes: &[u8]) -> Result<IWICBitmapFrameDecode> {
    unsafe {
        let stream = SHCreateMemStream(Some(bytes)).ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_OUTOFMEMORY))?;
        let dec = f.CreateDecoderFromStream(&stream, std::ptr::null::<GUID>(), WICDecodeMetadataCacheOnDemand)?;
        dec.GetFrame(0)
    }
}

/// A decoded screenshot: PNG bytes and a thumbnail (premultiplied BGRA).
pub struct Decoded {
    pub png: Arc<Vec<u8>>,
    pub w: u32,
    pub h: u32,
    pub thumb: (u32, u32, Arc<Vec<u8>>),
}

/// Turns the clipboard's picture into PNG bytes and a thumbnail.
pub fn decode_raw(raw: Raw) -> Option<Decoded> {
    let f = factory().ok()?;
    let (bytes, is_png) = match raw {
        Raw::Png(b) => (b, true),
        Raw::Dib(d) => (super::bmp_file(&d)?, false),
    };
    unsafe {
        let frame = decode(&f, &bytes).ok()?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h).ok()?;
        if (w as u64) * (h as u64) > 100_000_000 {
            return None;
        }
        let (tw, th) = super::thumb_size(w, h)?;
        let scaler = f.CreateBitmapScaler().ok()?;
        scaler.Initialize(&frame, tw, th, WICBitmapInterpolationModeFant).ok()?;
        let conv = f.CreateFormatConverter().ok()?;
        conv.Initialize(&scaler, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).ok()?;
        let mut px = vec![0u8; crate::gfx::bgra_len(tw, th)?];
        conv.CopyPixels(std::ptr::null(), tw * 4, &mut px).ok()?;
        // A DIB's fourth byte is often 0 (not alpha): the picture is opaque.
        if !is_png && px.chunks_exact(4).all(|p| p[3] == 0) {
            for p in px.chunks_exact_mut(4) {
                p[3] = 255;
            }
        }
        let png = if is_png { bytes } else { encode_png(&f, &frame).ok()? };
        Some(Decoded { png: Arc::new(png), w, h, thumb: (tw, th, Arc::new(px)) })
    }
}

/// PNG bytes of `frame` (opaque: a DIB's unused fourth byte is no alpha).
unsafe fn encode_png(f: &IWICImagingFactory, frame: &IWICBitmapFrameDecode) -> Result<Vec<u8>> {
    let stream: IStream = SHCreateMemStream(None).ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_OUTOFMEMORY))?;
    let enc = f.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
    enc.Initialize(&stream, WICBitmapEncoderNoCache)?;
    let mut fe: Option<IWICBitmapFrameEncode> = None;
    enc.CreateNewFrame(&mut fe, std::ptr::null_mut())?;
    let fe = fe.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
    fe.Initialize(None)?;
    let (mut w, mut h) = (0u32, 0u32);
    frame.GetSize(&mut w, &mut h)?;
    fe.SetSize(w, h)?;
    // 24-bit BGR: what a screenshot is (no alpha to keep).
    let mut fmt = windows::Win32::Graphics::Imaging::GUID_WICPixelFormat24bppBGR;
    fe.SetPixelFormat(&mut fmt)?;
    let conv = f.CreateFormatConverter()?;
    conv.Initialize(frame, &fmt, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)?;
    fe.WriteSource(&conv, std::ptr::null())?;
    fe.Commit()?;
    enc.Commit()?;
    stream_bytes(&stream)
}

unsafe fn stream_bytes(s: &IStream) -> Result<Vec<u8>> {
    let mut st = STATSTG::default();
    s.Stat(&mut st, STATFLAG_NONAME)?;
    let n = st.cbSize as usize;
    if n > super::MAX_BYTES {
        return Err(windows::core::Error::from(windows::Win32::Foundation::E_OUTOFMEMORY));
    }
    s.Seek(0, STREAM_SEEK_SET, None)?;
    let mut out = vec![0u8; n];
    let mut got = 0u32;
    s.Read(out.as_mut_ptr() as *mut _, n as u32, Some(&mut got)).ok()?;
    out.truncate(got as usize);
    Ok(out)
}

/// A CF_DIB of the PNG `png` (for "Copy": apps that paste pictures read
/// that one; the PNG goes along too).
pub fn dib_from_png(png: &[u8]) -> Option<Vec<u8>> {
    let f = factory().ok()?;
    unsafe {
        let frame = decode(&f, png).ok()?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h).ok()?;
        let len = crate::gfx::bgra_len(w, h)?;
        if len > super::MAX_BYTES {
            return None;
        }
        let conv = f.CreateFormatConverter().ok()?;
        conv.Initialize(&frame, &GUID_WICPixelFormat32bppBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).ok()?;
        let mut px = vec![0u8; len];
        conv.CopyPixels(std::ptr::null(), w * 4, &mut px).ok()?;
        super::dib_of(w, h, &px)
    }
}
