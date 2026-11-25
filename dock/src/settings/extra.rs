//! The list pages: appearance (themes), and the About page.

use std::mem::ManuallyDrop;

use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{
    ID2D1DeviceContext, D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1,
    D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows_numerics::Vector2;

use super::rows::Mirror;
use super::{clip_to, App, Hit, FONT};
use crate::glyphs::Glyph;
use crate::i18n::t;

/// Built-in theme plus every valid file in %APPDATA%\NopBar\themes.
/// Deletes copied icons nothing in the saved config points at (after the
/// window closes: a cancelled pick must not leave files behind).
pub fn clean_icons() {
    let Some(cfg) = crate::config::read_current() else { return };
    let keep: Vec<String> = cfg.dock.icon_overrides.values().map(|v| v.to_lowercase()).collect();
    let dir = crate::config::app_dir().join("icons");
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let rel = format!("icons\\{}", e.file_name().to_string_lossy()).to_lowercase();
        if !keep.contains(&rel) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

pub fn list_themes() -> Vec<(String, String)> {
    let mut out = vec![("builtin:graphite".to_string(), t("settings.themes.default").to_string())];
    let dir = crate::config::app_dir().join("themes");
    let mut files: Vec<_> = std::fs::read_dir(&dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
    files.sort();
    for p in files {
        if p.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        let Ok(t) = serde_json::from_str::<crate::theme::Theme>(&text) else { continue };
        if t.validate().is_err() {
            continue;
        }
        let file = p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        let name = if t.name.is_empty() { file.clone() } else { t.name.clone() };
        out.push((format!("file:themes\\{file}"), name));
    }
    out
}

/// Height of `t` in the settings font at `size`, semibold when `bold`,
/// wrapped to `maxw` DIPs: what para_dir takes for the same text.
pub(super) fn wrapped_h(dw: &windows::Win32::Graphics::DirectWrite::IDWriteFactory, t: &str, size: f32, bold: bool, maxw: f32) -> f32 {
    use windows::Win32::Graphics::DirectWrite::{DWRITE_TEXT_METRICS, DWRITE_WORD_WRAPPING_WRAP};
    let weight = if bold { DWRITE_FONT_WEIGHT_SEMI_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL };
    let wide: Vec<u16> = crate::gfx::keep_words(t).encode_utf16().collect();
    let h = unsafe {
        (|| -> Result<f32> {
            // The shared format (the UI language's locale, Hangul in Malgun
            // Gothic, never Bold): what the page draws the text with.
            let format = crate::gfx::ui_format(dw, &windows::core::HSTRING::from(&*crate::fonts::map(FONT)), weight, size * crate::fonts::scale())?;
            let l = dw.CreateTextLayout(&wide, &format, maxw.max(1.0), 4000.0)?;
            l.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
            let mut m = DWRITE_TEXT_METRICS::default();
            l.GetMetrics(&mut m)?;
            Ok(m.height)
        })()
    };
    h.unwrap_or(size * 1.4)
}

/// Posted to the settings window when a pin icon finished loading.
pub const WM_ICON_READY: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 7;

pub const LINKS: [(&str, &str); 4] = [
    ("about.link.website", "https://nopbard.com/nopbar"),
    ("about.link.mail", "mailto:nopbard@gmail.com"),
    ("about.link.license", "https://www.gnu.org/licenses/gpl-3.0.html"),
    ("about.sponsor", super::SPONSORS_URL),
];

pub(super) fn open_url(url: &str) {
    crate::launch::open_soon(crate::launch::Target::new(url, url, crate::launch::Source::Settings));
}

impl App {
    /// Loads `src`'s icon on a short-lived thread (Shell image extraction can
    /// be slow); the window repaints when it arrives.
    fn want_icon(&self, src: String) {
        // A few at a time: stuck icon extensions keep their threads, new
        // pictures wait for a free one (audit A05). Not marked pending, so
        // a later paint asks again.
        static CAP: crate::worker::HelperCap = crate::worker::HelperCap::new(3);
        if self.icon_pending.borrow().contains(&src) {
            return;
        }
        let Some(slot) = CAP.try_take() else { return };
        self.icon_pending.borrow_mut().insert(src.clone());
        let px = (64.0 * self.scale.min(2.0)) as i32;
        let hwnd = self.hwnd.0 as isize;
        let done = self.icon_done.clone();
        let _ = std::thread::Builder::new().name("pin-icon".into()).spawn(move || {
            let _slot = slot;
            unsafe {
                let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
            }
            let icon = crate::icons::load(&src, px);
            if let Ok(mut d) = done.lock() {
                d.push((src, icon));
            }
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                    Some(windows::Win32::Foundation::HWND(hwnd as *mut _)),
                    WM_ICON_READY,
                    windows::Win32::Foundation::WPARAM(0),
                    windows::Win32::Foundation::LPARAM(0),
                );
            }
        });
    }

    /// Takes the icons the loader threads finished.
    pub(super) fn take_icons(&mut self) {
        let done: Vec<_> = self.icon_done.lock().map(|mut d| std::mem::take(&mut *d)).unwrap_or_default();
        let mut cache = self.pin_icons.borrow_mut();
        for (src, icon) in done {
            self.icon_pending.borrow_mut().remove(&src);
            cache.insert(src, icon);
        }
    }

    fn icon_source(&self, path: &str) -> String {
        self.cfg.dock.icon_overrides.get(path).and_then(|rel| crate::config::icon_file(rel)).unwrap_or_else(|| path.to_string())
    }

    /// A plain button with `label` on it.
    pub(super) unsafe fn button(&self, dc: &ID2D1DeviceContext, r: D2D_RECT_F, label: &str, hot: bool) -> Result<()> {
        self.button_emphasis(dc, r, label, hot, false)
    }

    pub(super) unsafe fn button_emphasis(&self, dc: &ID2D1DeviceContext, r: D2D_RECT_F, label: &str, hot: bool, primary: bool) -> Result<()> {
        self.note(super::layout::Kind::Button, label, r);
        // A tone step (no outline): the control fills, a step deeper under
        // the pointer, deeper still while held there (pressed).
        use crate::theme::State;
        let st = match (hot, self.pressed.is_some() && self.pressed == self.hover) {
            (true, true) => State::Pressed,
            (true, false) => State::Hover,
            _ => State::Rest,
        };
        let b = dc.CreateSolidColorBrush(&if primary { self.ink() } else { self.ctl_fill(st) }, None)?;
        dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: self.ctl_r(), radiusY: self.ctl_r() }, &b);
        let room = (r.right - r.left - 12.0).max(1.0);
        let (mut layout, mut tw, mut th) = self.gfx.text_layout(label, FONT, super::rows::tok::BODY, DWRITE_FONT_WEIGHT_NORMAL)?;
        if tw > room {
            // A long translation: a size smaller first (footnote 12: the least).
            (layout, tw, th) = self.gfx.text_layout(label, FONT, super::rows::tok::FOOTNOTE, DWRITE_FONT_WEIGHT_NORMAL)?;
        }
        let ink = dc.CreateSolidColorBrush(&if primary { if self.light { super::col(1.,1.,1.,1.) } else { super::col(0.08,0.08,0.08,1.) } } else { self.ink() }, None)?;
        let p = Vector2 { X: (r.left + r.right - tw.min(room)) / 2.0, Y: (r.top + r.bottom - th) / 2.0 };
        dc.PushAxisAlignedClip(&D2D_RECT_F { left: r.left + 4.0, right: r.right - 4.0, ..r }, windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        dc.DrawTextLayout(p, &layout, &ink, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
        dc.PopAxisAlignedClip();
        Ok(())
    }

    /// The same button with a glyph (12 DIP) on it in place of a label.
    pub(super) unsafe fn glyph_button(&self, dc: &ID2D1DeviceContext, r: D2D_RECT_F, g: Glyph, hot: bool) -> Result<()> {
        self.note(super::layout::Kind::Button, "", r);
        let b = dc.CreateSolidColorBrush(&self.ctl_fill(if hot { crate::theme::State::Hover } else { crate::theme::State::Rest }), None)?;
        dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: self.ctl_r(), radiusY: self.ctl_r() }, &b);
        self.glyph(dc, g, 12.0, (r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0, self.ink())
    }

    pub(super) unsafe fn label(&self, dc: &ID2D1DeviceContext, t: &str, x: f32, y: f32, size: f32, bold: bool, c: D2D1_COLOR_F) -> Result<f32> {
        let w = if bold { DWRITE_FONT_WEIGHT_SEMI_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL };
        let (layout, tw, _) = self.gfx.text_layout(t, FONT, size, w)?;
        self.note_text(t, D2D_RECT_F { left: x, top: y, right: x + tw, bottom: y + size * 1.25 });
        let b = dc.CreateSolidColorBrush(&c, None)?;
        dc.DrawTextLayout(Vector2 { X: x, Y: y }, &layout, &b, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
        Ok(tw)
    }

    /// Text wrapped to `maxw` DIPs; returns the height it took.
    pub(super) unsafe fn para(&self, dc: &ID2D1DeviceContext, t: &str, x: f32, y: f32, size: f32, bold: bool, maxw: f32, c: D2D1_COLOR_F) -> Result<f32> {
        self.para_dir(dc, t, x, y, size, bold, maxw, c, false)
    }

    /// Height of `t` wrapped to `maxw` DIPs.
    pub(super) fn para_h(&self, t: &str, size: f32, maxw: f32) -> f32 {
        wrapped_h(self.gfx.dwrite(), t, size, false, maxw)
    }

    /// para; `rtl`: read right to left (lines start at the right edge).
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn para_dir(&self, dc: &ID2D1DeviceContext, t: &str, x: f32, y: f32, size: f32, bold: bool, maxw: f32, c: D2D1_COLOR_F, rtl: bool) -> Result<f32> {
        use windows::Win32::Graphics::DirectWrite::{DWRITE_READING_DIRECTION_RIGHT_TO_LEFT, DWRITE_TEXT_METRICS, DWRITE_WORD_WRAPPING_WRAP};
        let w = if bold { DWRITE_FONT_WEIGHT_SEMI_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL };
        let (layout, _, _) = self.gfx.text_layout(&crate::gfx::keep_words(t), FONT, size, w)?;
        layout.SetMaxWidth(maxw.max(1.0))?;
        layout.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
        if rtl {
            layout.SetReadingDirection(DWRITE_READING_DIRECTION_RIGHT_TO_LEFT)?;
        }
        let mut m = DWRITE_TEXT_METRICS::default();
        layout.GetMetrics(&mut m)?;
        let lx = x + if rtl { maxw.max(1.0) - m.width } else { m.left };
        self.note_text(t, D2D_RECT_F { left: lx, top: y, right: lx + m.width, bottom: y + m.height });
        let b = dc.CreateSolidColorBrush(&c, None)?;
        dc.DrawTextLayout(Vector2 { X: x, Y: y }, &layout, &b, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
        Ok(m.height)
    }

    pub(super) fn card(&self, dc: &ID2D1DeviceContext, r: D2D_RECT_F) -> Result<()> {
        unsafe {
            let b = dc.CreateSolidColorBrush(&self.card_fill(), None)?;
            let rad = self.card_r();
            dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: rad, radiusY: rad }, &b);
        }
        Ok(())
    }

    pub(super) unsafe fn draw_icon(&self, dc: &ID2D1DeviceContext, path: &str, r: D2D_RECT_F) -> Result<()> {
        let src = self.icon_source(path);
        let cache = self.pin_icons.borrow();
        let img = match cache.get(&src) {
            Some(Some(img)) => img,
            // Tried and failed: leave the spot empty, do not ask again.
            Some(None) => return Ok(()),
            None => {
                drop(cache);
                self.want_icon(src);
                return Ok(());
            }
        };
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
            colorContext: ManuallyDrop::new(None),
        };
        let bmp = dc.CreateBitmap(D2D_SIZE_U { width: img.width, height: img.height }, Some(img.pixels.as_ptr() as _), img.width * 4, &props)?;
        dc.DrawBitmap(&bmp, Some(&r), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
        Ok(())
    }

    /// About: app and updates, followed by help and project links.
    pub(super) unsafe fn paint_about(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, y: f32, left: f32, right: f32, clip: D2D_RECT_F) -> Result<f32> {
        use super::rows::tok;
        let rect = |l: f32, t: f32, r: f32, b: f32| D2D_RECT_F { left: l, top: t, right: r, bottom: b };
        let t = crate::i18n::t;
        let rtl = crate::i18n::rtl();
        let m = Mirror { on: rtl, left, right };
        let (tl, cr) = (left + tok::ROW_PAD, right - tok::ROW_PAD);
        // The app, its updates under it in the same card (version and
        // where it stands read together).
        let card = self.card_row(dc, "about.app", y, 88.0, left, right, true, false)?;
        let y = card.top;
        if let Ok(exe) = std::env::current_exe() {
            self.draw_icon(dc, &exe.to_string_lossy(), m.r(rect(tl, y + 16.0, tl + 56.0, y + 72.0)))?;
        }
        let nx = tl + 56.0 + 16.0;
        let room = cr - nx;
        let name_w = self.text_w("NopBar", tok::TITLE_SIZE, true).min(room - tok::INFO_W);
        self.text_box(dc, "NopBar", m.span(nx, room), y + 18.0, tok::TITLE_SIZE, true, room, self.ink(), rtl)?;
        self.paint_info(dc, hits, super::tooltip::card_tip("about.free"), m.x(nx + name_w + tok::INFO_GAP + tok::INFO_D / 2.0), y + 18.0 + tok::TITLE_SIZE * 0.67, clip)?;
        let ver = crate::i18n::tf("about.version", &[("v", env!("CARGO_PKG_VERSION"))]);
        self.text_box(dc, &ver, m.span(nx, room), y + 48.0, tok::BODY, false, room, self.dim(), rtl)?;

        let end = self.paint_updates(dc, hits, card.bottom, left, right, clip, true)?;

        // Help: Report a problem (its "i": what it opens), the links.
        let row = self.card_row(dc, "about.report", end, tok::ROW_MIN, left, right, true, false)?;
        let cy = row.top + tok::ROW_MIN / 2.0;
        let from = hits.len();
        let b = self.row_button(dc, hits, m, "about.report", cr, cy, t("about.report.button"), Hit::ReportProblem, clip)?;
        self.row_text(dc, hits, "about.report", row, cy, b.left, t("about.report"), Some(super::tooltip::card_tip("about.report.hint")), clip)?;
        super::cards::info_first(hits, from);
        let mut y = row.bottom;
        for (i, (key, _)) in LINKS.iter().enumerate() {
            let row = self.card_row(dc, key, y, tok::ROW_MIN, left, right, false, i + 1 == LINKS.len())?;
            let cy = row.top + tok::ROW_MIN / 2.0;
            if self.hover == Some(Hit::Link(i)) {
                self.fill_segment(dc, self.row_fill(self.state_of(Hit::Link(i))), m.r(row), self.card_r(), false, i + 1 == LINKS.len())?;
            }
            let g = if rtl { Glyph::ChevronLeft } else { Glyph::ChevronRight };
            self.glyph(dc, g, 10.0, m.x(cr - 5.0), cy, self.dim())?;
            self.row_text(dc, hits, key, row, cy, cr - 10.0, t(key), None, clip)?;
            if let Some(c) = clip_to(m.r(row), clip) {
                hits.push((c, Hit::Link(i)));
            }
            y = row.bottom;
        }

        Ok(y)
    }

    pub(super) fn click_extra(&mut self, h: Hit) {
        match h {
            Hit::Link(i) => {
                if let Some((_, url)) = LINKS.get(i) {
                    open_url(url);
                }
                return;
            }
            Hit::Theme(i) => {
                let Some((src, _)) = self.themes.get(i).cloned() else { return };
                if self.cfg.theme.source == src {
                    return;
                }
                self.cfg.theme.source = src;
                self.touch("theme");
            }
            _ => return,
        }
        self.send_preview();
    }
}
