//! The window's picture drawn offscreen into a PNG (examples/preview.rs,
//! tests): what the owner sees without running the setup. Framed: on a
//! plain desktop-like backdrop with rounded corners and a shadow, roughly
//! as Windows 11 shows the window.

use std::path::Path;

use windows::core::{Result, HSTRING};
use windows::Win32::Foundation::GENERIC_WRITE;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Factory, ID2D1Geometry, ID2D1RenderTarget, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_LAYER_OPTIONS_NONE, D2D1_LAYER_PARAMETERS, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
    D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppPBGRA, IWICBitmap, IWICBitmapFrameEncode, IWICImagingFactory,
    WICBitmapCacheOnLoad, WICBitmapEncoderNoCache,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows_numerics::Matrix3x2;

use super::draw::{self, rect, Resources, Scene, H, W};
use super::theme::{palette, Rgba};
use crate::strings::{self, Failure, Lang};

/// The states worth looking at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Shot {
    Ready,
    /// Ready, after Tab: the focus ring on the folder.
    Focused,
    /// The icon dragged over the folder.
    Dragging,
    /// Installing, the bar at this fraction.
    Busy(f32),
    Failed,
}

/// The scene for a shot.
pub fn scene(shot: Shot, lang: Lang) -> Scene<'static> {
    let mut s = Scene::ready();
    match shot {
        Shot::Ready => {}
        Shot::Focused => s.focus = true,
        Shot::Dragging => {
            s.icon = (draw::FOLDER.0 - 70.0, draw::FOLDER.1 + 6.0);
            s.folder_scale = 1.04;
        }
        Shot::Busy(p) => {
            s.icon_alpha = 0.0;
            s.progress = Some(p);
            s.hint = false;
            s.close_enabled = false;
        }
        Shot::Failed => {
            s.hint = false;
            s.message = Some(strings::failure(lang, Failure::Code(7)));
        }
    }
    s
}

/// Draws `shot` into a bitmap at `scale` (2.0 = 192 DPI): the window on a
/// backdrop `pad` DIP wide.
pub fn render(wic: &IWICImagingFactory, factory: &ID2D1Factory, dark: bool, lang: Lang, shot: Shot, scale: f32, pad: f32) -> Result<IWICBitmap> {
    let (cw, ch) = (W + 2.0 * pad, H + 2.0 * pad);
    unsafe {
        let bmp = wic.CreateBitmap((cw * scale).round() as u32, (ch * scale).round() as u32, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad)?;
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0 * scale,
            dpiY: 96.0 * scale,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let rt: ID2D1RenderTarget = factory.CreateWicBitmapRenderTarget(&bmp, &props)?;
        let icon = draw::load_icon(&rt, wic)?;
        let res = Resources::new(factory.clone(), lang)?;
        let pal = palette(dark, None);
        rt.BeginDraw();
        rt.Clear(Some(&(if dark { Rgba::hex(0x101012) } else { Rgba::hex(0xD5D8DE) }).d2d()));
        if pad > 0.0 {
            // DWM's shadow, roughly.
            for i in 0..14 {
                let g = 1.0 + i as f32 * 2.2;
                let t = 1.0 - i as f32 / 14.0;
                let b = rt.CreateSolidColorBrush(&Rgba::hex(0).alpha(if dark { 0.10 } else { 0.035 } * t * t).d2d(), None)?;
                let r = rect(pad - g, pad - g + 8.0, pad + W + g, pad + H + g + 8.0);
                rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: 8.0 + g, radiusY: 8.0 + g }, &b);
            }
        }
        // Windows 11 rounds the window's corners (8 DIP).
        let clip = factory.CreateRoundedRectangleGeometry(&D2D1_ROUNDED_RECT { rect: rect(pad, pad, pad + W, pad + H), radiusX: 8.0, radiusY: 8.0 })?;
        let geometry: ID2D1Geometry = windows::core::Interface::cast(&clip)?;
        rt.PushLayer(
            &D2D1_LAYER_PARAMETERS {
                contentBounds: rect(0.0, 0.0, cw, ch),
                geometricMask: std::mem::ManuallyDrop::new(Some(geometry)),
                maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                maskTransform: Matrix3x2::identity(),
                opacity: 1.0,
                opacityBrush: std::mem::ManuallyDrop::new(None),
                layerOptions: D2D1_LAYER_OPTIONS_NONE,
            },
            None,
        );
        let base = Matrix3x2 { M11: 1.0, M12: 0.0, M21: 0.0, M22: 1.0, M31: pad, M32: pad };
        let drawn = draw::draw(&rt, &icon, &res, &pal, strings::table(lang), &scene(shot, lang), base);
        rt.PopLayer();
        rt.EndDraw(None, None)?;
        drawn?;
        Ok(bmp)
    }
}

pub fn save_png(wic: &IWICImagingFactory, bmp: &IWICBitmap, path: &Path) -> Result<()> {
    unsafe {
        let stream = wic.CreateStream()?;
        stream.InitializeFromFilename(&HSTRING::from(path.as_os_str()), GENERIC_WRITE.0)?;
        let enc = wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
        enc.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let mut frame: Option<IWICBitmapFrameEncode> = None;
        enc.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
        let frame = frame.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
        frame.Initialize(None)?;
        let (mut w, mut h) = (0, 0);
        bmp.GetSize(&mut w, &mut h)?;
        frame.SetSize(w, h)?;
        let mut fmt = GUID_WICPixelFormat32bppPBGRA;
        frame.SetPixelFormat(&mut fmt)?;
        frame.WriteSource(bmp, std::ptr::null())?;
        frame.Commit()?;
        enc.Commit()?;
    }
    Ok(())
}

/// Every shot, light and dark, Korean (and English and Arabic Ready, Arabic
/// Failed), into `dir`.
/// Returns the files written.
pub fn render_all(dir: &Path, scale: f32) -> Result<Vec<std::path::PathBuf>> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let _ = std::fs::create_dir_all(dir);
        let mut out = Vec::new();
        let ko = Lang::from_tag("ko").unwrap_or(Lang::EN);
        let ar = Lang::from_tag("ar").unwrap_or(Lang::EN);
        let shots = [
            ("ready", Shot::Ready, ko),
            ("drag", Shot::Dragging, ko),
            ("busy", Shot::Busy(0.58), ko),
            // The finished window holds the full bar before it closes.
            ("done", Shot::Busy(1.0), ko),
            ("failed", Shot::Failed, ko),
            ("focus", Shot::Focused, ko),
            ("ready-en", Shot::Ready, Lang::EN),
            ("ready-ar", Shot::Ready, ar),
            ("failed-ar", Shot::Failed, ar),
        ];
        for dark in [false, true] {
            for (name, shot, lang) in shots {
                let bmp = render(&wic, &factory, dark, lang, shot, scale, 48.0)?;
                let p = dir.join(format!("setup-{}-{name}.png", if dark { "dark" } else { "light" }));
                save_png(&wic, &bmp, &p)?;
                out.push(p);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strings::TAGS;
    use crate::ui::draw::Text;
    use windows::Win32::Foundation::E_FAIL;

    fn lang(tag: &str) -> Lang {
        Lang::from_tag(tag).unwrap()
    }

    /// The bitmap's pixels (BGRA, premultiplied), row after row.
    fn pixels(bmp: &IWICBitmap) -> (u32, u32, Vec<u8>) {
        unsafe {
            let (mut w, mut h) = (0, 0);
            bmp.GetSize(&mut w, &mut h).unwrap();
            let mut px = vec![0u8; (w * h * 4) as usize];
            bmp.CopyPixels(std::ptr::null(), w * 4, &mut px).unwrap();
            (w, h, px)
        }
    }

    #[test]
    fn setup_window_draws_the_app_icon_at_supported_dpis() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).unwrap();
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).unwrap();
            for scale in [1.0, 1.25, 1.5, 2.0, 2.5] {
                for dark in [false, true] {
                    let (w, _, px) = pixels(&render(&wic, &factory, dark, Lang::EN, Shot::Ready, scale, 0.0).unwrap());
                    let (cx, cy) = draw::ICON_HOME;
                    let at = |u: f32, v: f32| {
                        let x = ((cx + draw::ICON * (u - 0.5)) * scale) as u32;
                        let y = ((cy + draw::ICON * (v - 0.5)) * scale) as u32;
                        let i = ((y * w + x) * 4) as usize;
                        &px[i..i + 4]
                    };
                    // The hat, nose and beard are flat, opaque brand colors.
                    assert_eq!(at(0.5, 0.25), [82, 96, 206, 255], "hat at {scale}x, dark {dark}");
                    assert_eq!(at(0.5, 0.625), [146, 185, 254, 255], "nose at {scale}x, dark {dark}");
                    assert_eq!(at(0.5, 0.8125), [252, 253, 253, 255], "beard at {scale}x, dark {dark}");
                    assert_eq!(at(0.125, 0.125), if dark { [31, 30, 30, 255] } else { [255; 4] }, "transparent corner at {scale}x, dark {dark}");
                }
            }
        }
    }

    #[test]
    fn every_state_draws_in_both_modes_and_in_high_contrast() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).unwrap();
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).unwrap();
            for dark in [false, true] {
                for shot in [Shot::Ready, Shot::Focused, Shot::Dragging, Shot::Busy(0.0), Shot::Busy(0.5), Shot::Busy(1.0), Shot::Failed] {
                    for lang in [lang("ko"), Lang::EN, lang("ar"), lang("he")] {
                        let bmp = render(&wic, &factory, dark, lang, shot, 1.5, 0.0).unwrap();
                        let (mut w, mut h) = (0, 0);
                        bmp.GetSize(&mut w, &mut h).unwrap();
                        assert_eq!((w, h), (690, 420));
                    }
                }
            }
            // High contrast: the outlined palette draws too.
            let hc = crate::ui::theme::HighContrast { window: Rgba::hex(0), text: Rgba::hex(0xFFFF00), highlight: Rgba::hex(0x1AEBFF) };
            let bmp = wic.CreateBitmap(460, 280, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad).unwrap();
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                ..Default::default()
            };
            let rt = factory.CreateWicBitmapRenderTarget(&bmp, &props).unwrap();
            let icon = draw::load_icon(&rt, &wic).unwrap();
            let res = Resources::new(factory.clone(), lang("ko")).unwrap();
            rt.BeginDraw();
            draw::draw(&rt, &icon, &res, &palette(false, Some(hc)), strings::table(lang("ko")), &scene(Shot::Failed, lang("ko")), Matrix3x2::identity()).unwrap();
            rt.EndDraw(None, None).unwrap();
        }
    }

    /// Every language's words fit where they go: each failure on its one
    /// line, the hint across the window, Retry inside its pill.
    #[test]
    fn every_language_fits() {
        let factory: ID2D1Factory = unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).unwrap() };
        let mut over = Vec::new();
        for tag in TAGS {
            let lang = lang(tag);
            let res = Resources::new(factory.clone(), lang).unwrap();
            let s = strings::table(lang);
            for f in [Failure::NoPayload, Failure::Extract, Failure::Start, Failure::Code(1), Failure::Code(7), Failure::Code(8)] {
                let m = strings::failure(lang, f);
                let wide = res.message_width(m);
                if wide > draw::MESSAGE.right - draw::MESSAGE.left {
                    over.push(format!("{tag}: {m}: {wide} DIP"));
                }
            }
            let hint = res.width(s.hint, Text::Hint);
            if hint > W - 48.0 {
                over.push(format!("{tag}: {}: {hint} DIP", s.hint));
            }
            let retry = res.width(s.retry, Text::Button);
            if retry > draw::RETRY.right - draw::RETRY.left - 12.0 {
                over.push(format!("{tag}: {}: {retry} DIP", s.retry));
            }
        }
        assert!(over.is_empty(), "too wide:
{}", over.join("
"));
    }

    /// Right to left the picture is the mirror image, its icon unflipped:
    /// the Arabic window's icon is the English one moved to the right, its
    /// folder moved to the left, its close button at the top left. (An
    /// offscreen bitmap, no window.)
    #[test]
    fn right_to_left_is_mirrored() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).unwrap();
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).unwrap();
            for dark in [false, true] {
                let (w, h, en) = pixels(&render(&wic, &factory, dark, Lang::EN, Shot::Ready, 1.0, 0.0).unwrap());
                let (_, _, ar) = pixels(&render(&wic, &factory, dark, lang("ar"), Shot::Ready, 1.0, 0.0).unwrap());
                assert_eq!((w, h), (W as u32, H as u32));
                let at = |px: &[u8], x: u32, y: u32| -> [u8; 4] {
                    let i = ((y * w + x) * 4) as usize;
                    [px[i], px[i + 1], px[i + 2], px[i + 3]]
                };
                // A box around each, and the same box on the other side
                // (moved by the mirrored distance, not flipped).
                let same = |cx: u32, cy: u32, half: u32, to: u32| -> std::result::Result<(), windows::core::Error> {
                    for y in cy - half..cy + half {
                        for x in cx - half..cx + half {
                            if at(&en, x, y) != at(&ar, x + to - cx, y) {
                                return Err(E_FAIL.into());
                            }
                        }
                    }
                    Ok(())
                };
                let icon = (draw::ICON_HOME.0 as u32, draw::ICON_HOME.1 as u32);
                let folder = (draw::FOLDER.0 as u32, draw::FOLDER.1 as u32);
                assert!(same(icon.0, icon.1, 52, W as u32 - icon.0).is_ok(), "the icon moved, unflipped (dark {dark})");
                assert!(same(folder.0, folder.1, 52, W as u32 - folder.0).is_ok(), "the folder moved (dark {dark})");
                assert!(same(icon.0, icon.1, 40, icon.0).is_err(), "not where the English one is (dark {dark})");
                // The close glyph: top right in English, top left in Arabic.
                assert!(same(W as u32 - 22, 15, 10, 22).is_ok(), "close at the top left (dark {dark})");
                // The arrow points the other way: its head (the barbs above
                // the shaft) is on the right in English, on the left in Arabic.
                let (mid, ay) = ((W / 2.0) as u32, draw::ICON_HOME.1 as u32);
                let ink = |px: &[u8], x: u32| (x - 1..=x + 1).any(|x| (ay - 5..=ay - 3).any(|y| at(px, x, y) != at(px, x, ay - 20)));
                assert!(ink(&en, mid + 19) && !ink(&en, mid - 19), "English points right");
                assert!(ink(&ar, mid - 19) && !ink(&ar, mid + 19), "Arabic points left");
            }
        }
    }
}
