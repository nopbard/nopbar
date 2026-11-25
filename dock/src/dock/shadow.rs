//! The bar's drop shadow (the theme's `dock.shadows`, fable review 6 look A).
//!
//! It lives in a window of its own, the dock's size and place, just under
//! it in the z-order, click-through (layered + transparent) and never
//! active. In the dock's own window the hit region (input.rs apply_region:
//! the bar at rest) would cut the shadow off at the bar's edge, and a
//! larger region would take clicks meant for the windows around the bar.
//!
//! The picture is computed once per look (layers, bar thickness, corner
//! radius, scale, orientation): the bar's rounded rectangle, blurred and
//! offset per layer, with the bar itself cut out (the glass shows no
//! shadow through it). It is a short stand-in bar; a nine-grid brush
//! stretches its middle column to the bar's length, so magnification only
//! moves and resizes one sprite. Everything in this window is in physical
//! pixels (no root scale): the nine-grid's insets are then plain pixels.

use windows::core::{w, Interface, Result, PCWSTR};
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{CompositionNineGridBrush, CompositionStretch, ContainerVisual, SpriteVisual};
use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_numerics::{Vector2, Vector3};

use crate::gfx::Gfx;
use crate::theme::Rgba;

/// One layer (DIP): offset, blur (like CSS: the Gaussian's sigma is half
/// of it) and color.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Layer {
    pub ox: f32,
    pub oy: f32,
    pub blur: f32,
    pub color: Rgba,
}

/// What the shadow is drawn for (DIP). No layers: no shadow.
#[derive(Clone, Debug, PartialEq, Default)]
pub(super) struct Spec {
    pub layers: Vec<Layer>,
    pub radius: f32,
    /// The bar's thickness across the dock.
    pub thick: f32,
    /// The contact shadow (theme::contact): a ring theme::CONTACT_W wide
    /// just past the bar, no blur. None: none.
    pub contact: Option<Rgba>,
}

/// The computed picture: premultiplied BGRA, with how far it reaches past
/// the bar on each side and the nine-grid's insets (px).
#[derive(Debug)]
pub(super) struct Picture {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u8>,
    /// Past the bar: left, top, right, bottom.
    pub pad: [u32; 4],
    /// Nine-grid insets: left, top, right, bottom.
    pub insets: [u32; 4],
}

/// Signed distance (px) from (x, y) to a rounded rectangle.
fn rounded_sdf(x: f32, y: f32, l: f32, t: f32, w: f32, h: f32, r: f32) -> f32 {
    let (cx, cy) = (l + w / 2.0, t + h / 2.0);
    let qx = (x - cx).abs() - (w / 2.0 - r);
    let qy = (y - cy).abs() - (h / 2.0 - r);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - r
}

/// Separable Gaussian blur of a `w` x `h` plane (zero past the edges).
fn blur(src: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    if sigma < 0.3 {
        return src.to_vec();
    }
    let rad = (sigma * 3.0).ceil() as i32;
    let mut k: Vec<f32> = (-rad..=rad).map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp()).collect();
    let sum: f32 = k.iter().sum();
    k.iter_mut().for_each(|v| *v /= sum);
    let mut tmp = vec![0.0f32; w * h];
    for y in 0..h {
        let row = &src[y * w..(y + 1) * w];
        for x in 0..w {
            let mut acc = 0.0;
            for (j, kv) in k.iter().enumerate() {
                let sx = x as i32 + j as i32 - rad;
                if sx >= 0 && (sx as usize) < w {
                    acc += row[sx as usize] * kv;
                }
            }
            tmp[y * w + x] = acc;
        }
    }
    let mut out = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0;
            for (j, kv) in k.iter().enumerate() {
                let sy = y as i32 + j as i32 - rad;
                if sy >= 0 && (sy as usize) < h {
                    acc += tmp[sy as usize * w + x] * kv;
                }
            }
            out[y * w + x] = acc;
        }
    }
    out
}

/// The shadow of a short stand-in bar at `scale`; its middle column
/// (along the dock) is what the nine-grid stretches. None: nothing to draw.
pub(super) fn picture(spec: &Spec, scale: f32, vertical: bool) -> Option<Picture> {
    if spec.layers.is_empty() || spec.thick <= 0.0 {
        return None;
    }
    let s = scale.max(0.5);
    let thick = (spec.thick * s).round().max(2.0);
    let r = (spec.radius * s).clamp(0.0, thick / 2.0);
    let sigma = |l: &Layer| l.blur * s / 2.0;
    let reach = |l: &Layer| (sigma(l) * 3.0).ceil();
    let far = spec.layers.iter().map(reach).fold(0.0f32, f32::max);
    // Long enough that its middle is a straight edge's shadow (no corner
    // reaches it), odd so that middle is one column.
    let mut along = 2.0 * r.ceil() + 2.0 * far + 3.0;
    if (along as u32) % 2 == 0 {
        along += 1.0;
    }
    let (pw, ph) = if vertical { (thick, along) } else { (along, thick) };
    let side = |f: &dyn Fn(&Layer) -> f32| spec.layers.iter().map(|l| (reach(l) + f(l)).max(0.0)).fold(0.0f32, f32::max).ceil();
    let (pl, pt) = (side(&|l| -(l.ox * s).round()), side(&|l| -(l.oy * s).round()));
    let (pr, pb) = (side(&|l| (l.ox * s).round()), side(&|l| (l.oy * s).round()));
    let (w, h) = ((pw + pl + pr) as usize, (ph + pt + pb) as usize);
    if w * h > 4_000_000 {
        return None;
    }
    // The bar's coverage, anti-aliased.
    let mut mask = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let d = rounded_sdf(x as f32 + 0.5, y as f32 + 0.5, pl, pt, pw, ph, r);
            mask[y * w + x] = (0.5 - d).clamp(0.0, 1.0);
        }
    }
    // Each blur once, however many layers share it.
    let mut blurred: Vec<(f32, Vec<f32>)> = Vec::new();
    for l in &spec.layers {
        let sg = sigma(l);
        if !blurred.iter().any(|(b, _)| (*b - sg).abs() < 1e-3) {
            blurred.push((sg, blur(&mask, w, h, sg)));
        }
    }
    // Layers over each other in order (premultiplied), then the bar cut out.
    let mut acc = vec![[0.0f32; 4]; w * h];
    for l in &spec.layers {
        let sg = sigma(l);
        let Some((_, b)) = blurred.iter().find(|(x, _)| (*x - sg).abs() < 1e-3) else { continue };
        let (ox, oy) = ((l.ox * s).round() as i32, (l.oy * s).round() as i32);
        let a0 = l.color.a as f32 / 255.0;
        let rgb = [l.color.r as f32 / 255.0, l.color.g as f32 / 255.0, l.color.b as f32 / 255.0];
        for y in 0..h as i32 {
            let sy = y - oy;
            if sy < 0 || sy >= h as i32 {
                continue;
            }
            for x in 0..w as i32 {
                let sx = x - ox;
                if sx < 0 || sx >= w as i32 {
                    continue;
                }
                let a = a0 * b[sy as usize * w + sx as usize];
                if a <= 0.0 {
                    continue;
                }
                let p = &mut acc[y as usize * w + x as usize];
                for c in 0..3 {
                    p[c] = rgb[c] * a + p[c] * (1.0 - a);
                }
                p[3] = a + p[3] * (1.0 - a);
            }
        }
    }
    // The contact shadow over them: the bar grown by CONTACT_W (the bar
    // itself is cut out below, so a ring is left).
    if let Some(c) = spec.contact.filter(|c| c.a > 0) {
        let grow = crate::theme::CONTACT_W * s;
        let a0 = c.a as f32 / 255.0;
        let rgb = [c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0];
        for y in 0..h {
            for x in 0..w {
                let d = rounded_sdf(x as f32 + 0.5, y as f32 + 0.5, pl, pt, pw, ph, r) - grow;
                let a = a0 * (0.5 - d).clamp(0.0, 1.0);
                if a <= 0.0 {
                    continue;
                }
                let p = &mut acc[y * w + x];
                for k in 0..3 {
                    p[k] = rgb[k] * a + p[k] * (1.0 - a);
                }
                p[3] = a + p[3] * (1.0 - a);
            }
        }
    }
    let mut px = vec![0u8; w * h * 4];
    for (i, p) in acc.iter().enumerate() {
        let keep = 1.0 - mask[i];
        let q = |v: f32| (v * keep * 255.0).round().clamp(0.0, 255.0) as u8;
        // BGRA, premultiplied.
        px[i * 4] = q(p[2]);
        px[i * 4 + 1] = q(p[1]);
        px[i * 4 + 2] = q(p[0]);
        px[i * 4 + 3] = q(p[3]);
    }
    let (pl, pt, pr, pb) = (pl as u32, pt as u32, pr as u32, pb as u32);
    let (w, h) = (w as u32, h as u32);
    // The middle column (row, on a side dock) stretches; nothing else does.
    let insets = if vertical {
        let top = pt + ph as u32 / 2;
        [0, top, 0, h - top - 1]
    } else {
        let left = pl + pw as u32 / 2;
        [left, 0, w - left - 1, 0]
    };
    Some(Picture { w, h, px, pad: [pl, pt, pr, pb], insets })
}

/// Where the shadow slides to with the dock (auto-hide), kept in DIP so
/// the pixel offset can be worked out again for a new scale or picture.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Slide {
    pub x: f32,
    pub y: f32,
}

impl Slide {
    /// The offset in pixels at `scale` for a picture reaching `pad` past
    /// the bar: the dock's slide, and further by that reach so none of the
    /// shadow stays on the screen's edge.
    pub fn px(self, scale: f32, pad: [u32; 4]) -> (f32, f32) {
        let reach = pad.iter().copied().max().unwrap_or(0) as f32;
        let more = |d: f32| if d > 0.0 { d * scale + reach } else if d < 0.0 { d * scale - reach } else { 0.0 };
        (more(self.x), more(self.y))
    }
}

/// The shadow window and what it shows.
pub(super) struct BarShadow {
    hwnd: HWND,
    _target: DesktopWindowTarget,
    /// Follows the dock's slide (auto-hide), in px.
    slide: ContainerVisual,
    /// The dock's slide now (DIP), and the pixel offset last set for it.
    slid: Slide,
    slid_px: Option<(f32, f32)>,
    scale: f32,
    sprite: SpriteVisual,
    brush: CompositionNineGridBrush,
    /// What the picture was drawn for (None: nothing drawn).
    drawn: Option<(Spec, u32, bool)>,
    pad: [u32; 4],
    /// The window is shown (a picture is drawn, and not held). Under a
    /// fullscreen app it stays shown, right under the dock, out of the
    /// topmost band with it.
    shown: bool,
    /// Not shown yet: the dock is still being built, before its place in
    /// the z-order is known (a fullscreen app may be in front); see
    /// Dock::reveal.
    held: bool,
}

unsafe extern "system" fn shadow_proc(hwnd: HWND, msg: u32, wp: windows::Win32::Foundation::WPARAM, lp: windows::Win32::Foundation::LPARAM) -> windows::Win32::Foundation::LRESULT {
    match msg {
        // Never active, never hit.
        WM_MOUSEACTIVATE => windows::Win32::Foundation::LRESULT(MA_NOACTIVATE as isize),
        WM_NCHITTEST => windows::Win32::Foundation::LRESULT(HTTRANSPARENT as isize),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

impl BarShadow {
    pub(super) unsafe fn new(gfx: &Gfx) -> Result<Self> {
        let hinst = GetModuleHandleW(None)?;
        let class = w!("NopBarDockShadow");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(shadow_proc),
            hInstance: hinst.into(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        // Click-through, never active, no taskbar button; no redirection
        // surface: only the shadow is drawn.
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_LAYERED,
            class,
            PCWSTR::null(),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(hinst.into()),
            None,
        )?;
        let made = (|| -> Result<Self> {
            SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA)?;
            let c = &gfx.compositor;
            let interop: ICompositorDesktopInterop = c.cast()?;
            let target = interop.CreateDesktopWindowTarget(hwnd, true)?;
            let root = c.CreateContainerVisual()?;
            target.SetRoot(&root)?;
            let slide = c.CreateContainerVisual()?;
            root.Children()?.InsertAtTop(&slide)?;
            let sprite = c.CreateSpriteVisual()?;
            sprite.SetIsVisible(false)?;
            slide.Children()?.InsertAtTop(&sprite)?;
            let brush = c.CreateNineGridBrush()?;
            sprite.SetBrush(&brush)?;
            Ok(Self { hwnd, _target: target, slide, sprite, brush, drawn: None, pad: [0; 4], shown: false, held: true, slid: Slide::default(), slid_px: None, scale: 1.0 })
        })();
        if made.is_err() {
            let _ = DestroyWindow(hwnd);
        }
        made
    }

    /// The slide container (its implicit Offset animation is the dock's).
    pub(super) fn slide(&self) -> &ContainerVisual {
        &self.slide
    }

    /// The shadow itself: its opacity is its strength (crate::motion::
    /// light: it comes up with the dock back from auto-hide).
    pub(super) fn sprite(&self) -> &SpriteVisual {
        &self.sprite
    }

    /// The dock slid by (`x`, `y`) DIP (auto-hide; 0 = back): the shadow
    /// goes with it, further by its own reach so none of it is left on
    /// the screen's edge.
    pub(super) fn slide_to(&mut self, x: f32, y: f32, scale: f32) {
        self.slid = Slide { x, y };
        self.scale = scale;
        self.apply_slide();
    }

    /// Sets the slide's pixel offset for the scale and picture now (a new
    /// DPI or look while hidden needs another one), only when it changed.
    fn apply_slide(&mut self) {
        let px = self.slid.px(self.scale, self.pad);
        if self.slid_px != Some(px) {
            self.slid_px = Some(px);
            let _ = self.slide.SetOffset(Vector3 { X: px.0, Y: px.1, Z: 0.0 });
        }
    }

    /// The dock moved or resized (window rect `r`, px): the shadow window
    /// takes the same place, just under it.
    pub(super) unsafe fn place(&self, dock: HWND, r: RECT) {
        let show = if self.shown { SWP_SHOWWINDOW } else { SWP_HIDEWINDOW };
        if let Err(e) = SetWindowPos(self.hwnd, Some(dock), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOACTIVATE | show) {
            crate::log!("dock shadow: cannot place ({e:?})");
        }
    }

    /// Back under the dock (it was brought to the top).
    pub(super) unsafe fn restack(&self, dock: HWND) {
        if self.shown {
            let _ = SetWindowPos(self.hwnd, Some(dock), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
    }

    /// The dock is shown, in its place: the shadow may show too (right
    /// under it, in the same step).
    pub(super) unsafe fn release(&mut self, dock: HWND) {
        self.held = false;
        self.sync_shown(dock);
    }

    unsafe fn sync_shown(&mut self, dock: HWND) {
        let want = !self.held && self.drawn.is_some();
        if want == self.shown {
            return;
        }
        self.shown = want;
        if want {
            let _ = SetWindowPos(self.hwnd, Some(dock), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
        } else {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    /// The device was replaced: draw the picture again at the next layout.
    pub(super) fn forget(&mut self) {
        self.drawn = None;
    }

    /// Shows the shadow under the bar at `bar` (window DIP) for `spec`
    /// (no layers: none). Draws the picture only when the look changed.
    pub(super) unsafe fn update(&mut self, gfx: &Gfx, dock: HWND, spec: &Spec, bar: (f32, f32, f32, f32), vertical: bool, scale: f32) {
        let key_scale = (scale * 1000.0).round() as u32;
        let fresh = self.drawn.as_ref().is_some_and(|(s, k, v)| s == spec && *k == key_scale && *v == vertical);
        if !fresh {
            self.drawn = None;
            let _ = self.sprite.SetIsVisible(false);
            if let Some(p) = picture(spec, scale, vertical) {
                match self.show_picture(gfx, &p) {
                    Ok(()) => {
                        self.pad = p.pad;
                        self.drawn = Some((spec.clone(), key_scale, vertical));
                        let _ = self.sprite.SetIsVisible(true);
                    }
                    Err(e) => crate::log!("dock shadow: cannot draw ({e:?})"),
                }
            }
            self.sync_shown(dock);
        }
        // The picture's reach or the scale may have changed: the slide too.
        self.scale = scale;
        self.apply_slide();
        if self.drawn.is_none() {
            return;
        }
        let (x, y, w, h) = (bar.0 * scale, bar.1 * scale, bar.2 * scale, bar.3 * scale);
        let [pl, pt, pr, pb] = self.pad.map(|v| v as f32);
        // Across the dock the picture is drawn 1:1; along it, stretched.
        // (Not snapped: it moves with the bar, which is not either.)
        let (w, h) = if vertical { (pl + pr + w.round(), h + pt + pb) } else { (w + pl + pr, pt + pb + h.round()) };
        let _ = self.sprite.SetOffset(Vector3 { X: x - pl, Y: y - pt, Z: 0.0 });
        let _ = self.sprite.SetSize(Vector2 { X: w, Y: h });
    }

    unsafe fn show_picture(&self, gfx: &Gfx, p: &Picture) -> Result<()> {
        let surface = gfx.surface_from_bgra(p.w, p.h, &p.px)?;
        let source = gfx.compositor.CreateSurfaceBrushWithSurface(&surface)?;
        source.SetStretch(CompositionStretch::Fill)?;
        self.brush.SetSource(&source)?;
        let [l, t, r, b] = p.insets.map(|v| v as f32);
        self.brush.SetInsetsWithValues(l, t, r, b)?;
        self.brush.SetIsCenterHollow(false)?;
        Ok(())
    }
}

impl Drop for BarShadow {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// Pictures for the design audit (tests only).
#[cfg(test)]
#[path = "shadow_harmony.rs"]
mod harmony;

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> Spec {
        let black = |a| Rgba { r: 0, g: 0, b: 0, a };
        Spec { layers: vec![Layer { ox: 0.0, oy: 2.0, blur: 6.0, color: black(0x59) }, Layer { ox: 0.0, oy: 10.0, blur: 28.0, color: black(0x40) }], radius: 18.0, thick: 68.0, contact: None }
    }

    fn alpha(p: &Picture, x: u32, y: u32) -> u8 {
        p.px[((y * p.w + x) * 4 + 3) as usize]
    }

    #[test]
    fn no_layers_no_picture() {
        assert!(picture(&Spec { layers: Vec::new(), radius: 18.0, thick: 68.0, contact: None }, 1.0, false).is_none());
    }

    #[test]
    fn shadow_falls_down_and_spares_the_glass() {
        let p = picture(&spec(), 1.0, false).unwrap();
        let [pl, pt, pr, pb] = p.pad;
        // Light from above: it reaches further below the bar than above.
        assert!(pb > pt && pl == pr, "{:?}", p.pad);
        assert_eq!(p.px.len(), (p.w * p.h * 4) as usize);
        let mid = pl + (p.w - pl - pr) / 2;
        // Inside the bar: nothing (the glass shows no shadow).
        assert_eq!(alpha(&p, mid, pt + 34), 0);
        // Just under it: dark; far under it: fading out.
        let under = alpha(&p, mid, pt + 68 + 3);
        let farther = alpha(&p, mid, pt + 68 + 30);
        assert!(under > farther && farther > 0, "{under} {farther}");
        // Just above it: lighter than just under it.
        assert!(alpha(&p, mid, pt - 3) < under);
        // The edge of the picture is (almost) clear: nothing is cut off.
        assert!(alpha(&p, mid, p.h - 1) <= 1 && alpha(&p, 0, pt + 34) <= 1);
        // The middle column is what stretches.
        assert_eq!(p.insets[0], mid);
        assert_eq!(p.insets[0] + 1 + p.insets[2], p.w);
        assert_eq!(p.insets[1] + p.insets[3], 0);
    }

    #[test]
    fn the_middle_column_is_a_straight_edge() {
        // Next to the middle column the shadow is the same: stretching it
        // along the bar changes nothing.
        let p = picture(&spec(), 1.5, false).unwrap();
        let mid = p.insets[0];
        for y in 0..p.h {
            let (a, b) = (alpha(&p, mid, y) as i32, alpha(&p, mid + 1, y) as i32);
            assert!((a - b).abs() <= 1, "row {y}: {a} vs {b}");
        }
    }

    #[test]
    fn hidden_shadow_stays_off_screen_after_a_dpi_change() {
        // A bottom dock hidden at 100 %, then the display goes to 200 %:
        // the offset is worked out again from the slide kept in DIP, with
        // the new picture's reach, and the whole shadow is past the
        // window's bottom edge.
        let (depth, bar_y) = (52.0 * 1.4 + 8.0 + 8.0 + 44.0, 52.0 * 1.4 + 44.0 - 8.0);
        let slide = Slide { x: 0.0, y: depth - bar_y + 4.0 };
        for scale in [1.0, 1.25, 2.0, 3.0] {
            let p = picture(&spec(), scale, false).unwrap();
            let (_, dy) = slide.px(scale, p.pad);
            let top = bar_y * scale - p.pad[1] as f32 + dy;
            assert!(top >= depth * scale, "{scale}: shadow top {top} inside the window {}", depth * scale);
        }
        // The old scale's offset would not do at the new one.
        let small = picture(&spec(), 1.0, false).unwrap();
        let big = picture(&spec(), 2.0, false).unwrap();
        assert!(slide.px(1.0, small.pad).1 < slide.px(2.0, big.pad).1);
        // Shown: no offset whatever the scale.
        assert_eq!(Slide::default().px(2.0, big.pad), (0.0, 0.0));
        // Side docks slide sideways, past their own edge.
        assert!(Slide { x: -80.0, y: 0.0 }.px(2.0, big.pad).0 < -160.0);
    }

    #[test]
    fn side_docks_stretch_rows() {
        let p = picture(&spec(), 1.0, true).unwrap();
        assert_eq!(p.insets[0] + p.insets[2], 0);
        assert_eq!(p.insets[1] + 1 + p.insets[3], p.h);
        // Still lit from above.
        assert!(p.pad[3] > p.pad[1]);
    }
}
