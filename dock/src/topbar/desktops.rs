//! The top bar's virtual desktop cell: one dot per desktop (filled = the
//! current one, a ring = the others; more than MAX_DOTS shows "3/12"),
//! and the small name tip under it while the pointer rests on a dot.
//!
//! The desktops themselves (registry, watching, switching) are crate::vdesk.

use std::cell::RefCell;
use std::rc::Rc;

use windows::core::{w, Interface, Result};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_ROUNDED_RECT};
use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{CompositionStretch, SpriteVisual};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_numerics::{Vector2, Vector3};

use super::{CompositionSurfaceBrush, Style, TopBar, GLYPH_BOX};
use crate::gfx::Gfx;
use crate::theme::{Rgba, Theme};
use crate::vdesk::Desktops;

#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub(super) enum TipTarget {Desktop(usize),Companion,Tray(u32, u32)}
fn tip_target(companion:bool,warning:bool,desktop:Option<usize>,menu:bool)->Option<TipTarget>{
    if menu {None} else if companion&&warning {Some(TipTarget::Companion)} else {desktop.map(TipTarget::Desktop)}
}

/// More desktops than this show as "current/total" text.
pub(super) const MAX_DOTS: usize = 9;
/// Dot diameter and center-to-center distance (DIP).
pub(super) const DOT: f32 = 6.0;
pub(super) const PITCH: f32 = 12.0;
/// Ring line width (DIP).
const RING: f32 = 1.25;
/// The pointer rests this long on the cell before the first tip (the
/// kit's wait, 6.3); after that the tip follows it from dot to dot at once,
/// and within motion::TIP_WARM_MS of a tip closing the next one does not
/// wait (tip_is_warm).
pub(crate) const TIP_DELAY_MS: u32 = crate::motion::TIP_DELAY_MS as u32;
/// A tip to the bar it hangs from (DIP): a surface to what opened it.
pub(super) const TIP_GAP: f32 = crate::theme::metrics::size::EDGE_GAP;

/// What the cell shows.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Look {
    Hidden,
    /// `rtl`: desktop 1 is the rightmost dot.
    Dots { n: usize, current: Option<usize>, rtl: bool },
    /// "3/12".
    Count(String),
}

/// What a bar shows for `d`. `mode`: index into config::DESKTOPS_MODES
/// (auto / always / never); `here`: this bar shows desktops at all (the
/// primary bar, or every bar with same_on_all_displays).
pub(super) fn look(mode: usize, here: bool, d: &Desktops, rtl: bool) -> Look {
    let n = d.count();
    if !here || mode == 2 || n == 0 || (mode == 0 && n <= 1) {
        return Look::Hidden;
    }
    if n <= MAX_DOTS {
        Look::Dots { n, current: d.current.filter(|&c| c < n), rtl }
    } else {
        let cur = d.current.filter(|&c| c < n).map(|c| (c + 1).to_string()).unwrap_or_else(|| "–".into());
        Look::Count(format!("{cur}/{n}"))
    }
}

/// Width of `n` dots (DIP).
pub(super) fn dots_width(n: usize) -> f32 {
    if n == 0 { 0.0 } else { (n - 1) as f32 * PITCH + DOT }
}

/// Center of desktop i's dot from the dots' left edge (DIP).
pub(super) fn dot_x(i: usize, n: usize, rtl: bool) -> f32 {
    let slot = if rtl { n.saturating_sub(1).saturating_sub(i) } else { i };
    DOT / 2.0 + slot as f32 * PITCH
}

/// The desktop whose dot is nearest `x` (DIP from the dots' left edge;
/// before the first or past the last dot counts as that dot).
pub(super) fn dot_at(x: f32, n: usize, rtl: bool) -> usize {
    if n == 0 {
        return 0;
    }
    let slot = ((x - DOT / 2.0) / PITCH).round().clamp(0.0, (n - 1) as f32) as usize;
    if rtl { n - 1 - slot } else { slot }
}

fn d2d(c: Rgba, alpha: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.r as f32 / 255.0, g: c.g as f32 / 255.0, b: c.b as f32 / 255.0, a: c.a as f32 / 255.0 * alpha }
}

/// Draws the dots (`n`, the current one, right to left) in a surface
/// `h_px` tall with its top left at (`ox`, `oy`) px: the current desktop
/// filled in `fill`, the others rings in `ring`, at `s` px per DIP.
#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn paint_dots(dc: &windows::Win32::Graphics::Direct2D::ID2D1DeviceContext, ox: f32, oy: f32, h_px: f32, dots: (usize, Option<usize>, bool), fill: D2D1_COLOR_F, ring: D2D1_COLOR_F, s: f32) -> Result<()> {
    dots_in(dc, ox, oy, h_px, dots, fill, ring, s)
}

/// paint_dots' dots alone.
#[allow(clippy::too_many_arguments)]
unsafe fn dots_in(dc: &windows::Win32::Graphics::Direct2D::ID2D1DeviceContext, ox: f32, oy: f32, h_px: f32, (n, current, rtl): (usize, Option<usize>, bool), fill: D2D1_COLOR_F, ring: D2D1_COLOR_F, s: f32) -> Result<()> {
    let fill_b = dc.CreateSolidColorBrush(&fill, None)?;
    let ring_b = dc.CreateSolidColorBrush(&ring, None)?;
    let cy = oy + h_px / 2.0;
    for i in 0..n {
        let cx = ox + dot_x(i, n, rtl) * s;
        if current == Some(i) {
            let r = DOT / 2.0 * s;
            dc.FillEllipse(&D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: r, radiusY: r }, &fill_b);
        } else {
            let r = (DOT - RING) / 2.0 * s;
            dc.DrawEllipse(&D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: r, radiusY: r }, &ring_b, RING * s, None);
        }
    }
    Ok(())
}

impl TopBar {
    /// What this bar's desktop cell shows now.
    pub(super) fn desktops_look(&self) -> Look {
        let here = self.full();
        look(self.cfg.desktops_mode(), here, &self.desktops, crate::i18n::rtl())
    }

    /// The dots, drawn in the bar's text color into one GLYPH_BOX-high
    /// surface (cached like the other cells).
    pub(super) fn dots_surface(&mut self, n: usize, current: Option<usize>, rtl: bool) -> Option<(CompositionSurfaceBrush, f32, f32)> {
        let k = (Style::Glyph, format!("\u{0}desktops|{n}|{current:?}|{rtl}"));
        if let Some(s) = self.surfaces.get(&k) {
            return Some(s.clone());
        }
        let s = self.scale;
        let wd = dots_width(n);
        let (w_px, h_px) = ((wd * s).ceil().max(1.0) as u32, (GLYPH_BOX * s).round().max(1.0) as u32);
        let ink = self.ground.ink;
        // High contrast: the ring in full text color like the dot; else a
        // touch softer so the filled one leads.
        let ring_alpha = if self.theme.high_contrast { 1.0 } else { 0.8 };
        let (fill, ring) = (d2d(ink, 1.0), d2d(ink, ring_alpha));
        let surface = self.gfx.draw(w_px, h_px, |dc, ox, oy| unsafe { paint_dots(dc, ox, oy, h_px as f32, (n, current, rtl), fill, ring, s) }).ok()?;
        let brush = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface).ok()?;
        let _ = brush.SetStretch(CompositionStretch::Fill);
        let size = (w_px as f32 / s, h_px as f32 / s);
        self.surfaces.insert(k, (brush.clone(), size.0, size.1));
        Some((brush, size.0, size.1))
    }

    /// The desktop under bar x (DIP) if it is over the desktop cell: the
    /// dot's desktop, or with the "3/12" text the current one.
    pub(super) fn desktop_at(&self, x: f32) -> Option<usize> {
        let i = self.hit(x)?;
        let s = self.slots.get(i).filter(|s| s.cell == super::Cell::Desktops)?;
        match self.desktops_look() {
            Look::Dots { n, rtl, .. } => {
                let left = s.x + (s.w - s.content.0) / 2.0;
                Some(dot_at(x - left, n, rtl))
            }
            Look::Count(_) => Some(self.desktops.current.unwrap_or(0)),
            Look::Hidden => None,
        }
    }

    /// The tip's text for desktop i.
    fn tip_text(&self, i: usize) -> String {
        let name = self.desktops.name(i);
        match self.desktops_look() {
            Look::Count(_) => crate::i18n::tf(
                "topbar.desktops.position",
                &[("name", &name), ("i", &(i + 1).to_string()), ("n", &self.desktops.count().to_string())],
            ),
            _ => name,
        }
    }

    /// The pointer moved to bar x: start, move or drop the name tip.
    pub(super) fn track_tip(&mut self, x: Option<f32>) {
        let over_companion=self.slot_of(super::Cell::Companion).is_some_and(|i|{
            let r=self.slot_rect(i);
            x.is_some_and(|x|{let sx=x*self.scale+self.at.left as f32;sx>=r.left as f32&&sx<r.right as f32})
        });
        let want=tip_target(over_companion,crate::companion::tooltip().is_some(),
            x.and_then(|x|self.desktop_at(x)),crate::popup::is_tracking()).or_else(|| {
                if crate::popup::is_tracking() || !crate::taskbar::hidden_by_us() { return None; }
                let e = self.tray_at(x?)?;
                e.standard_tip().then_some(TipTarget::Tray(e.owner, e.uid))
            });
        match want {
            None => self.drop_tip(),
            Some(d) if self.tip_live => {
                if self.tip != Some(d) {
                    self.tip = Some(d);
                    self.show_tip();
                }
            }
            Some(d) => {
                let arrived = self.tip.is_none();
                self.tip = Some(d);
                match tip_start(arrived, tip_is_warm()) {
                    TipStart::Now => {
                        unsafe {
                            let _ = KillTimer(Some(self.hwnd), super::TIP_TIMER);
                        }
                        self.tip_live = true;
                        self.show_tip();
                    }
                    TipStart::Wait => unsafe {
                        SetTimer(Some(self.hwnd), super::TIP_TIMER, TIP_DELAY_MS, None);
                    },
                    TipStart::Waiting => {}
                }
            }
        }
    }

    /// The wait is over: show the tip if its hover target is still valid.
    pub(super) fn tip_due(&mut self) {
        if self.tip.is_some() {
            self.tip_live = true;
            self.refresh_tip();
        }
    }

    /// Hide the tip and forget the visit (the next one waits again).
    pub(super) fn drop_tip(&mut self) {
        if self.tip.is_some() || self.tip_live {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), super::TIP_TIMER);
            }
            hide_tip();
        }
        self.tip = None;
        self.tip_live = false;
    }

    /// The desktops or the language changed under a shown tip.
    pub(super) fn refresh_tip(&mut self) {
        if !self.tip_live {
            return;
        }
        let valid=match self.tip {
            Some(TipTarget::Desktop(d))=>self.desktops_look()!=Look::Hidden && d<self.desktops.count(),
            Some(TipTarget::Companion)=>crate::companion::tooltip().is_some() && self.slot_of(super::Cell::Companion).is_some(),
            Some(TipTarget::Tray(owner, uid)) => crate::taskbar::hidden_by_us() && self.tray.iter().any(|e| e.owner == owner && e.uid == uid && e.standard_tip()),
            None=>false,
        };
        if !valid {
            self.drop_tip();
        } else {
            self.show_tip();
        }
    }

    fn show_tip(&self) {
        let Some(target)=self.tip else{return;};
        if let TipTarget::Tray(owner, uid) = target {
            let Some((k, e)) = self.tray.iter().enumerate().find(|(_, e)| e.owner == owner && e.uid == uid && e.standard_tip()) else { return };
            let Some(i) = self.slot_of(super::Cell::Tray(k)) else { return };
            let r = self.slot_rect(i);
            let (at, up) = tip_anchor((r.left+r.right)/2, r, (TIP_GAP*self.scale).round() as i32, self.cfg.at_bottom());
            super::show_tip_wrapped(&self.gfx, &self.theme, self.scale, &e.tip, at, up, self.tip_bounds(), 320.*self.scale);
            return;
        }
        if target==TipTarget::Companion {
            let (Some(i),Some(text))=(self.slot_of(super::Cell::Companion),crate::companion::tooltip()) else{return;};
            let r=self.slot_rect(i);
            let (at,up)=tip_anchor((r.left+r.right)/2,r,(TIP_GAP*self.scale).round() as i32,self.cfg.at_bottom());
            super::show_tip_wrapped(&self.gfx,&self.theme,self.scale,&text,at,up,self.tip_bounds(),320.*self.scale);
            return;
        }
        let TipTarget::Desktop(d)=target else{return;};
        let Some(i) = self.slot_of(super::Cell::Desktops) else { return };
        let r = self.slot_rect(i);
        let s = &self.slots[i];
        // Under the dot (or the text's middle).
        let cx = match self.desktops_look() {
            Look::Dots { n, rtl, .. } => s.x + (s.w - s.content.0) / 2.0 + dot_x(d, n, rtl),
            _ => s.x + s.w / 2.0,
        };
        let x = self.at.left + (cx * self.scale).round() as i32;
        let (at, up) = tip_anchor(x, r, (TIP_GAP * self.scale).round() as i32, self.cfg.at_bottom());
        show_tip_at(&self.gfx, &self.theme, self.scale, &self.tip_text(d), at, up, self.tip_bounds());
    }

    /// Where the bar's tips stay: its own display's work area now (the
    /// bar's monitor, never guessed from the tip's point), or that
    /// monitor whole while it cannot be read.
    pub(super) fn tip_bounds(&self) -> RECT {
        crate::monitors::info(self.hmon).map(|m| m.work).unwrap_or(self.monitor)
    }
}

// ---- the tip window -------------------------------------------------------------

struct Tip {
    hwnd: HWND,
    _target: DesktopWindowTarget,
    visual: SpriteVisual,
    /// Fading out: the crate::motion::after that hides it at the end.
    hiding: Option<usize>,
}

/// What the pointer coming onto something with a tip starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TipStart {
    /// The tip shows at once: one closed a moment ago (the warm state).
    Now,
    /// The wait starts (TIP_DELAY_MS).
    Wait,
    /// Already waiting: nothing new.
    Waiting,
}

/// Pure: what the pointer on something with a tip starts (the tip is not
/// up). `arrived`: it just came onto it (nothing is waited for yet);
/// `warm`: a tip closed within motion::TIP_WARM_MS (tip_is_warm).
pub(super) fn tip_start(arrived: bool, warm: bool) -> TipStart {
    match (warm, arrived) {
        (true, _) => TipStart::Now,
        (false, true) => TipStart::Wait,
        (false, false) => TipStart::Waiting,
    }
}

thread_local! {
    /// When the last tip went (hide_tip): the warm state's clock.
    static TIP_CLOSED: std::cell::Cell<Option<std::time::Instant>> = const { std::cell::Cell::new(None) };
}

/// A tip closed a moment ago (motion::tip_warm): the next one, on whatever
/// it is, shows without its wait (6.3).
pub(super) fn tip_is_warm() -> bool {
    crate::motion::tip_warm(TIP_CLOSED.with(|c| c.get()).map(|t| t.elapsed()))
}

/// How a tip shown now fades.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TipFade {
    /// Not up: it fades in (crate::motion::FADE_IN_MS).
    In,
    /// Fading out: it turns back from where it is.
    Back,
    /// Up (following the pointer from dot to dot): only its text changes.
    Stay,
}

/// Pure: how a tip shown now fades, `up`: its window shows; `hiding`: it
/// is fading out.
pub(super) fn tip_fade(up: bool, hiding: bool) -> TipFade {
    if hiding {
        TipFade::Back
    } else if up {
        TipFade::Stay
    } else {
        TipFade::In
    }
}

thread_local! {
    static TIP: RefCell<Option<Tip>> = const { RefCell::new(None) };
}

unsafe extern "system" fn tip_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe fn make_tip(gfx: &Rc<Gfx>) -> Result<Tip> {
    let hinst = GetModuleHandleW(None)?;
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(tip_proc),
        hInstance: hinst.into(),
        lpszClassName: w!("NopBarTip"),
        ..Default::default()
    };
    RegisterClassExW(&wc);
    let hwnd = CreateWindowExW(
        WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
        w!("NopBarTip"),
        w!("Desktop name"),
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
    let made = (|| -> Result<Tip> {
        let interop: ICompositorDesktopInterop = gfx.compositor.cast()?;
        let target = interop.CreateDesktopWindowTarget(hwnd, true)?;
        let visual = gfx.compositor.CreateSpriteVisual()?;
        target.SetRoot(&visual)?;
        Ok(Tip { hwnd, _target: target, visual, hiding: None })
    })();
    if made.is_err() {
        let _ = DestroyWindow(hwnd);
    }
    made
}

/// Where a tip for bar cell `r` goes, centered on `x` (screen px): `gap`
/// under the cell, its top edge there; or, for a bar at the bottom
/// (`up`), `gap` over it, its bottom edge there. (point, up)
pub(super) fn tip_anchor(x: i32, r: RECT, gap: i32, up: bool) -> (POINT, bool) {
    (POINT { x, y: if up { r.top - gap } else { r.bottom + gap } }, up)
}

/// The tip's top-left corner for a label `w` x `h` px centered on `at`
/// (its top edge, or its bottom edge when `up`), kept inside `bounds` on
/// both axes (a bar's tips: its own display's work area, so a tip over a
/// bar at the bottom stays off a dock reserving the space above it).
pub(super) fn tip_origin(at: POINT, w: i32, h: i32, up: bool, bounds: RECT) -> (i32, i32) {
    let x = (at.x - w / 2).clamp(bounds.left, (bounds.right - w).max(bounds.left));
    let y = if up { at.y - h } else { at.y };
    (x, y.min(bounds.bottom - h).max(bounds.top))
}

/// The tip's text (the ramp's Footnote) and its padding (S3 across, S2
/// down).
const TIP_TEXT: f32 = crate::theme::ramp::FOOTNOTE.size;
const TIP_PAD: (f32, f32) = (crate::theme::metrics::space::S3, crate::theme::metrics::space::S2);

/// A tip's label in px: its size, its padding and the margin around it
/// for its shadow (E1's key: offset plus blur), for text `tw` x `th` px.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct TipBox {
    pub w: u32,
    pub h: u32,
    pub pad_x: f32,
    pub pad_y: f32,
    pub margin: u32,
}

/// Pure: the label for text `tw` x `th` px at `scale` (whole pixels);
/// room around it for E1 only when it has a shadow (`shadows`:
/// tip_shadows is not empty).
pub(super) fn tip_box(tw: f32, th: f32, scale: f32, shadows: bool) -> TipBox {
    let (pad_x, pad_y) = ((TIP_PAD.0 * scale).round(), (TIP_PAD.1 * scale).round());
    let margin = if shadows { (crate::theme::Elevation::E1.reach() * scale).ceil() as u32 } else { 0 };
    TipBox { w: (tw + pad_x * 2.0).ceil() as u32, h: (th + pad_y * 2.0).ceil() as u32, pad_x, pad_y, margin }
}

/// The tip's corners: the theme's tooltip radius (8).
pub(super) fn tip_radius(theme: &Theme) -> f32 {
    theme.radii().tip
}

/// The tip's shadows (DIP): E1; none in high contrast or with the
/// "Shadows" effect off (`on`: crate::effects::items().shadows), like the
/// dock's name label and the panels (design B2 review P3 #3). Every drawn
/// tooltip takes them from here (the settings window's chip too).
pub(crate) fn tip_shadows(theme: &Theme, on: bool) -> Vec<(f32, f32, f32, crate::theme::Rgba)> {
    if on {
        theme.elevation(crate::theme::Elevation::E1)
    } else {
        Vec::new()
    }
}

/// Shows `text` in a small label centered under `at` (screen px, its top
/// edge; over it, `at` its bottom edge, when `up`), kept inside `bounds`.
pub(crate) fn show_tip_at(gfx: &Rc<Gfx>, theme: &Theme, scale: f32, text: &str, at: POINT, up: bool, bounds: RECT) {
    show_tip(gfx, theme, scale, text, at, up, bounds, None);
}

/// Lines a wrapped tip (show_tip_wrapped) keeps at most ("…" after).
const TIP_LINES: u32 = 8;

/// Like show_tip_at for a text that may be long (a window's whole title):
/// wider than `max_px` it wraps there (TIP_LINES at most) instead of
/// running on or being cut to one line.
#[allow(clippy::too_many_arguments)]
pub(crate) fn show_tip_wrapped(gfx: &Rc<Gfx>, theme: &Theme, scale: f32, text: &str, at: POINT, up: bool, bounds: RECT, max_px: f32) {
    show_tip(gfx, theme, scale, text, at, up, bounds, Some(max_px));
}

/// Uses the same layout and padding as the common tooltip, in device pixels.
pub(crate) fn wrapped_tip_size(gfx: &Gfx, theme: &Theme, scale: f32, text: &str, max_px: f32) -> Option<(u32,u32)> {
    let a=tip_art(gfx,theme,scale,text,Some(max_px))?;
    Some((a.b.w,a.b.h))
}

/// What a tip's label is drawn from (px): its text laid out, its box, its
/// corners, shadows and colors: material M4 (the tips' tint), the rim and
/// the contact shadow; high contrast's outline instead of that light.
pub(super) struct TipArt {
    layout: windows::Win32::Graphics::DirectWrite::IDWriteTextLayout,
    pub b: TipBox,
    rad: f32,
    shadows: Vec<(f32, f32, f32, Rgba)>,
    bg: D2D1_COLOR_F,
    fg: D2D1_COLOR_F,
    /// The rim (theme::rim; None in high contrast) and the contact shadow
    /// (theme::contact; clear in high contrast).
    rim: Option<[Rgba; 3]>,
    /// The rim's width, px (Theme::rim_width at `scale`: one device
    /// pixel; Flat v2's 1 DIP).
    rim_w: f32,
    contact: Rgba,
    /// Flat v2's fall from top to bottom (Theme::flat_fill; None: `bg`).
    fall: Option<(Rgba, Rgba)>,
    /// High contrast's outline: (color, DIP), inside the edge.
    edge: Option<(D2D1_COLOR_F, f32)>,
    scale: f32,
}

/// The label for `text` at `scale` (wider than `max_px` it wraps there:
/// see show_tip_wrapped).
pub(super) fn tip_art(gfx: &Gfx, theme: &Theme, scale: f32, text: &str, max_px: Option<f32>) -> Option<TipArt> {
    let (size, weight) = (TIP_TEXT * scale, DWRITE_FONT_WEIGHT_NORMAL);
    let (mut layout, mut tw, mut th) = gfx.text_layout(text, &theme.font.family, size, weight).ok()?;
    if let Some(max) = max_px.map(f32::floor).filter(|m| tw > *m) {
        let (l, h) = gfx.text_layout_wrapped_leading(text, &theme.font.family, size, weight, max, TIP_LINES).ok()?;
        (layout, tw, th) = (l, max, h);
    }
    // E1 under the label (none in high contrast or with the "Shadows"
    // effect off): DIP layers in px.
    let shadows: Vec<_> = tip_shadows(theme, crate::effects::items().shadows).into_iter().map(|(x, y, blur, c)| (x * scale, y * scale, blur * scale, c)).collect();
    let b = tip_box(tw, th, scale, !shadows.is_empty());
    let (bg, fg) = theme.label_colors();
    let rad = tip_radius(theme) * scale;
    // A tip is a raised plate: no outline but high contrast's (12.3).
    let edge = theme.outline_of(crate::theme::Role::Raised, crate::theme::State::Rest).map(|(c, w)| (d2d(c, 1.0), w));
    let (bg, fg) = (d2d(bg, 1.0), d2d(fg, 1.0));
    let (rim, fall) = (theme.rim_of(crate::theme::Material::Tip), theme.flat_fill(crate::theme::Material::Tip));
    let rim_w = theme.rim_width(crate::theme::Material::Tip, scale) * scale;
    Some(TipArt { layout, b, rad, shadows, bg, fg, rim, rim_w, contact: theme.contact(), fall, edge, scale })
}

/// Draws the label `a` (px), its shadow margin's top left at (`ox`, `oy`):
/// E1, the contact shadow, the body, the rim (rim_w: one device pixel,
/// Flat v2's 1 DIP; high contrast: its outline), the text. The light is the shared one
/// (gfx::parts::contact, gfx::parts::rim).
pub(super) unsafe fn paint_tip(dc: &windows::Win32::Graphics::Direct2D::ID2D1DeviceContext, ox: f32, oy: f32, a: &TipArt) -> Result<()> {
    let (b, rad, scale) = (a.b, a.rad, a.scale);
    let m = b.margin as f32;
    let body = D2D_RECT_F { left: ox + m, top: oy + m, right: ox + m + b.w as f32, bottom: oy + m + b.h as f32 };
    crate::gfx::soft_shadows(dc, body, rad, &a.shadows)?;
    crate::gfx::parts::contact(dc, body, rad, a.contact, scale)?;
    let bg = Rgba { r: (a.bg.r * 255.0).round() as u8, g: (a.bg.g * 255.0).round() as u8, b: (a.bg.b * 255.0).round() as u8, a: (a.bg.a * 255.0).round() as u8 };
    crate::gfx::parts::plate_fill(dc, body, rad, a.fall, bg)?;
    if let Some(stops) = a.rim {
        crate::gfx::parts::rim(dc, body, rad, stops, a.rim_w)?;
    }
    // An outline only in high contrast (borderless otherwise).
    if let Some((e, w)) = a.edge {
        let w = (w * scale).max(1.0);
        let inner = D2D_RECT_F { left: body.left + w / 2.0, top: body.top + w / 2.0, right: body.right - w / 2.0, bottom: body.bottom - w / 2.0 };
        dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: inner, radiusX: (rad - w / 2.0).max(0.0), radiusY: (rad - w / 2.0).max(0.0) }, &dc.CreateSolidColorBrush(&e, None)?, w, None);
    }
    dc.DrawTextLayout(Vector2 { X: body.left + b.pad_x, Y: body.top + b.pad_y }, &a.layout, &dc.CreateSolidColorBrush(&a.fg, None)?, D2D1_DRAW_TEXT_OPTIONS_NONE);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn show_tip(gfx: &Rc<Gfx>, theme: &Theme, scale: f32, text: &str, at: POINT, up: bool, bounds: RECT, max_px: Option<f32>) {
    let drawn = (|| -> Option<(windows::UI::Composition::CompositionDrawingSurface, u32, u32, u32)> {
        let a = tip_art(gfx, theme, scale, text, max_px)?;
        let b = a.b;
        let (sw, sh) = (b.w + 2 * b.margin, b.h + 2 * b.margin);
        let surface = gfx.draw(sw, sh, |dc, ox, oy| unsafe { paint_tip(dc, ox, oy, &a) }).ok()?;
        Some((surface, b.w, b.h, b.margin))
    })();
    let Some((surface, w, h, margin)) = drawn else { return };
    TIP.with(|t| {
        let mut t = t.borrow_mut();
        if t.is_none() {
            match unsafe { make_tip(gfx) } {
                Ok(tip) => *t = Some(tip),
                Err(e) => {
                    crate::log!("topbar: desktop tip: {e}");
                    return;
                }
            }
        }
        let Some(tip) = t.as_mut() else { return };
        let Ok(brush) = gfx.compositor.CreateSurfaceBrushWithSurface(&surface) else { return };
        let _ = brush.SetStretch(CompositionStretch::Fill);
        let _ = tip.visual.SetBrush(&brush);
        let (ww, wh) = (w + 2 * margin, h + 2 * margin);
        let _ = tip.visual.SetSize(Vector2 { X: ww as f32, Y: wh as f32 });
        let _ = tip.visual.SetOffset(Vector3 { X: 0.0, Y: 0.0, Z: 0.0 });
        // The label where it belongs; the window around it holds its shadow.
        let (x, y) = tip_origin(at, w as i32, h as i32, up, bounds);
        let (x, y) = (x - margin as i32, y - margin as i32);
        let fade = tip_fade(unsafe { IsWindowVisible(tip.hwnd) }.as_bool(), tip.hiding.is_some());
        if let Some(id) = tip.hiding.take() {
            crate::motion::cancel(id);
        }
        unsafe {
            let _ = SetWindowPos(tip.hwnd, Some(HWND_TOPMOST), x, y, ww as i32, wh as i32, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        }
        // It comes on as a whole over FADE_IN_MS (a tip is small and
        // brief: its light does not come up apart, 7.2 rule 2); a short
        // fade with reduced motion (motion::pointer_ms).
        let c = &gfx.compositor;
        let ms = crate::motion::pointer_ms(crate::motion::now(), true);
        let faded = match fade {
            TipFade::Stay => true,
            TipFade::In => tip.visual.SetOpacity(0.0).is_ok() && crate::motion::fade_to(c, &tip.visual, 1.0, ms, crate::motion::EASE_OUT).is_ok(),
            TipFade::Back => crate::motion::fade_to(c, &tip.visual, 1.0, ms, crate::motion::EASE_OUT).is_ok(),
        };
        if !faded {
            let _ = tip.visual.StopAnimation(&windows::core::HSTRING::from("Opacity"));
            let _ = tip.visual.SetOpacity(1.0);
        }
    });
}

/// Hides the name tip (if shown): it fades out over FADE_OUT_MS (a short
/// fade with reduced motion), then hides; shown again meanwhile, it stays.
/// The next tip within motion::TIP_WARM_MS of this shows without its wait
/// (tip_is_warm).
pub(crate) fn hide_tip() {
    TIP.with(|t| {
        let Ok(mut t) = t.try_borrow_mut() else { return };
        let Some(tip) = t.as_mut() else { return };
        if tip.hiding.is_some() || !unsafe { IsWindowVisible(tip.hwnd) }.as_bool() {
            return;
        }
        TIP_CLOSED.with(|c| c.set(Some(std::time::Instant::now())));
        let ms = crate::motion::pointer_ms(crate::motion::now(), false);
        // The fade's own length (real time, the speed setting and reduced
        // motion already in it): the window hides once it has played.
        let faded = tip.visual.Compositor().ok().and_then(|c| crate::motion::fade_to(&c, &tip.visual, 0.0, ms, crate::motion::EASE_IN).ok());
        let Some(played) = faded else {
            unsafe {
                let _ = ShowWindow(tip.hwnd, SW_HIDE);
            }
            return;
        };
        let wait = hide_wait(played);
        tip.hiding = crate::motion::after(
            wait,
            Box::new(|| {
                TIP.with(|t| {
                    if let Ok(mut t) = t.try_borrow_mut() {
                        if let Some(tip) = t.as_mut().filter(|tip| tip.hiding.is_some()) {
                            tip.hiding = None;
                            unsafe {
                                let _ = ShowWindow(tip.hwnd, SW_HIDE);
                            }
                        }
                    }
                });
            }),
        );
    });
}

/// When a fading tip's window hides: once its fade (`played` ms of real
/// time, as motion::fade_to returned it) has played, and a frame later.
fn hide_wait(played: i64) -> u32 {
    played.max(0) as u32 + 16
}

#[cfg(test)]
mod tests {
    #[test]
    fn review_companion_warning_uses_the_common_tip_visit() {
        use super::{tip_target,TipTarget};
        assert_eq!(tip_target(true,true,None,false),Some(TipTarget::Companion));
        assert_eq!(tip_target(false,true,None,false),None); // pointer left
        assert_eq!(tip_target(true,false,None,false),None); // warning cancelled
        assert_eq!(tip_target(true,true,None,true),None); // menu opened
        assert_eq!(tip_target(false,true,Some(2),false),Some(TipTarget::Desktop(2)));
    }
    use super::*;

    #[test]
    fn a_fading_tip_hides_only_after_its_fade() {
        use crate::motion::{fade_plan, pointer_ms, Mode, EASE_IN};
        for mode in [Mode::Full, Mode::Reduced] {
            for speed in [0.25_f32, 1.0, 4.0] {
                let (played, _) = fade_plan(mode, pointer_ms(mode, false), EASE_IN, speed);
                let wait = hide_wait(played) as i64;
                assert!(wait >= played, "{mode:?} x{speed}: hides at {wait} before the fade ends at {played}");
                assert!(wait <= played + 16, "{mode:?} x{speed}: waits {wait} long after {played}");
            }
        }
        // Reduced motion: 60 ms whatever the speed, so the window waits 76.
        let (r, _) = fade_plan(Mode::Reduced, pointer_ms(Mode::Reduced, false), EASE_IN, 4.0);
        assert_eq!(hide_wait(r), (r + 16) as u32);
    }

    #[test]
    fn the_tip_is_a_borderless_8_dip_label_on_e1() {
        use crate::syslook::SysLook;
        for light in [false, true] {
            let t = crate::theme::Theme::graphite().resolve(&SysLook { light, ..Default::default() });
            assert_eq!(tip_radius(&t), t.radii().tip, "radius: the theme's tooltip");
            assert_eq!(tip_shadows(&t, true), t.elevation(crate::theme::Elevation::E1));
            assert_eq!(tip_shadows(&t, true).len(), 2, "near and far");
            let mut hc = t.clone();
            hc.high_contrast = true;
            assert!(tip_shadows(&hc, true).is_empty(), "high contrast: no shadow (its outline instead)");
        }
        assert_eq!(TIP_TEXT, crate::theme::ramp::FOOTNOTE.size);
        // Padding 8 x 4 and room for E1 (Elevation::reach: Flat v2's far
        // layer, y 3 + blur 9), whole pixels.
        for scale in [1.0f32, 1.25, 1.5, 2.0] {
            let b = tip_box(40.0 * scale, 16.0 * scale, scale, true);
            assert_eq!((b.pad_x, b.pad_y), ((8.0 * scale).round(), (4.0 * scale).round()));
            assert_eq!(b.margin, (crate::theme::Elevation::E1.reach() * scale).ceil() as u32);
            assert!(b.w >= (40.0 * scale) as u32 + 2 * b.pad_x as u32);
        }
        // No outline but in high contrast.
        let src = include_str!("desktops.rs");
        let body = src.split("#[cfg(test)]").next().unwrap();
        assert!(!body.contains("popup_border()"));
    }

    #[test]
    fn the_tip_s_shadow_follows_the_shadows_effect() {
        // Design B2 review P3 #3: "Shadows" off, no E1 (and no room kept
        // for it around the label); on, E1 as before.
        let t = crate::theme::Theme::graphite();
        assert!(tip_shadows(&t, false).is_empty());
        assert!(!tip_shadows(&t, true).is_empty());
        for scale in [1.0f32, 1.5, 2.0] {
            let off = tip_box(40.0, 16.0, scale, false);
            assert_eq!(off.margin, 0, "no shadow: no margin");
            assert_eq!((off.w, off.h), (tip_box(40.0, 16.0, scale, true).w, tip_box(40.0, 16.0, scale, true).h), "the label itself the same");
        }
        // The one drawing path reads the effect (show_tip).
        let src = include_str!("desktops.rs");
        let body = src.split("#[cfg(test)]").next().unwrap();
        assert!(body.contains("tip_shadows(theme, crate::effects::items().shadows)"));
    }

    #[test]
    fn tips_fade_in_once_and_turn_back_when_called_again() {
        assert_eq!(tip_fade(false, false), TipFade::In, "a fresh tip fades in");
        assert_eq!(tip_fade(true, false), TipFade::Stay, "from dot to dot: no new fade");
        assert_eq!(tip_fade(true, true), TipFade::Back, "fading out: back from where it is");
        // On over 90, off over 120; 60 either way with reduced motion.
        use crate::motion::{pointer_ms, Mode};
        assert_eq!((pointer_ms(Mode::Full, true), pointer_ms(Mode::Full, false)), (90, 120));
        assert_eq!((pointer_ms(Mode::Reduced, true), pointer_ms(Mode::Reduced, false)), (60, 60));
        // The one drawing path asks the kit (no length of its own).
        let src = include_str!("desktops.rs");
        let body = src.split("#[cfg(test)]").next().unwrap();
        assert!(body.contains("pointer_ms(crate::motion::now(), true)") && body.contains("pointer_ms(crate::motion::now(), false)"));
        assert!(!body.contains("QUICK_"));
    }

    #[test]
    fn a_tip_waits_once_and_not_at_all_while_warm() {
        // The kit's wait and gap.
        assert_eq!((TIP_DELAY_MS, TIP_GAP), (300, 8.0));
        // The pointer arrives: the wait starts once; while it runs nothing new.
        assert_eq!(tip_start(true, false), TipStart::Wait);
        assert_eq!(tip_start(false, false), TipStart::Waiting);
        // A tip closed a moment ago: this one shows at once, arrived or waiting.
        assert_eq!(tip_start(true, true), TipStart::Now);
        assert_eq!(tip_start(false, true), TipStart::Now);
        // Warm for TIP_WARM_MS after one closed; never before the first.
        use std::time::Duration;
        assert!(!crate::motion::tip_warm(None));
        assert!(crate::motion::tip_warm(Some(Duration::from_millis(499))) && !crate::motion::tip_warm(Some(Duration::from_millis(500))));
    }

    #[test]
    fn the_tip_carries_the_rim_and_the_contact_shadow() {
        use crate::syslook::SysLook;
        for light in [false, true] {
            let t = crate::theme::Theme::graphite().resolve(&SysLook { light, ..Default::default() });
            // Material M4, opaque on Flat; the light every raised plate has.
            assert_eq!(t.label_colors().0, t.material(crate::theme::Material::Tip));
            assert!(t.rim().is_some() && t.contact().a > 0);
            // The contact ring (0.5 DIP) fits the room kept for E1.
            assert!(crate::theme::CONTACT_W <= crate::theme::Elevation::E1.reach());
        }
        // Half of it in from the edge, concentric with the tip's corners:
        // one device pixel; Flat v2's 1 DIP (1.5 px at 150 %).
        let (inset, (w, h), r) = crate::gfx::parts::rim_stroke(90.0, 36.0, 12.0, 1.0);
        assert_eq!((inset, w, h, r), (0.5, 89.0, 35.0, 11.5));
        let (inset, (w, h), r) = crate::gfx::parts::rim_stroke(135.0, 54.0, 18.0, 1.5);
        assert_eq!((inset, w, h, r), (0.75, 133.5, 52.5, 17.25));
        // The paint draws them with the shared functions.
        let src = include_str!("desktops.rs");
        let body = src.split("#[cfg(test)]").next().unwrap();
        assert!(body.contains("crate::gfx::parts::contact(dc, body, rad, a.contact, scale)") && body.contains("crate::gfx::parts::rim(dc, body, rad, stops, a.rim_w)"));
        assert!(body.contains("theme.rim_width(crate::theme::Material::Tip, scale) * scale"));
    }

    fn desks(n: usize, current: Option<usize>) -> Desktops {
        Desktops { ids: (0..n).map(|i| [i as u8; 16]).collect(), names: vec![None; n], current }
    }

    #[test]
    fn shown_by_mode_and_count() {
        let (auto, always, never) = (0, 1, 2);
        assert_eq!(look(auto, true, &desks(1, Some(0)), false), Look::Hidden, "auto hides a lone desktop");
        assert_eq!(look(always, true, &desks(1, Some(0)), false), Look::Dots { n: 1, current: Some(0), rtl: false });
        assert_eq!(look(never, true, &desks(4, Some(0)), false), Look::Hidden);
        assert_eq!(look(auto, false, &desks(4, Some(0)), false), Look::Hidden, "not on this bar");
        assert_eq!(look(auto, true, &desks(3, Some(1)), true), Look::Dots { n: 3, current: Some(1), rtl: true });
        // A current index past the list is no current.
        assert_eq!(look(auto, true, &desks(2, Some(5)), false), Look::Dots { n: 2, current: None, rtl: false });
    }

    #[test]
    fn many_desktops_show_as_text() {
        assert_eq!(look(0, true, &desks(9, Some(8)), false), Look::Dots { n: 9, current: Some(8), rtl: false });
        assert_eq!(look(0, true, &desks(12, Some(2)), false), Look::Count("3/12".into()));
        assert_eq!(look(0, true, &desks(10, None), false), Look::Count("–/10".into()));
    }

    #[test]
    fn tips_hang_under_a_top_bar_and_over_a_bottom_one() {
        let inside = |(x, y): (i32, i32), w: i32, h: i32, b: RECT| x >= b.left && y >= b.top && x + w <= b.right && y + h <= b.bottom;
        // On top, the bar reserving its 32 px: 4 px under the cell.
        let work = RECT { left: 0, top: 32, right: 1920, bottom: 1080 };
        let (at, up) = tip_anchor(500, RECT { left: 480, top: 0, right: 520, bottom: 32 }, 4, false);
        assert_eq!((at, up), (POINT { x: 500, y: 36 }, false));
        assert_eq!(tip_origin(at, 60, 24, up, work), (470, 36));
        // At the bottom, nothing above the bar: 4 px over it.
        let work = RECT { left: 0, top: 0, right: 1920, bottom: 1048 };
        let cell = RECT { left: 480, top: 1048, right: 520, bottom: 1080 };
        let (at, up) = tip_anchor(500, cell, 4, true);
        assert_eq!((at, up), (POINT { x: 500, y: 1044 }, true));
        assert_eq!(tip_origin(at, 60, 24, up, work), (470, 1020));
        // A bottom dock reserving 70 px above the bar: the work area ends
        // at 978, and the tip stays over it, off the dock (not 1020..1044).
        let docked = RECT { bottom: 1080 - 32 - 70, ..work };
        let o = tip_origin(at, 60, 24, up, docked);
        assert_eq!(o, (470, 954));
        assert!(inside(o, 60, 24, docked));
        // A display above the main one (negative y), its bar at the bottom
        // and a dock reserving there too; a cell at its right end.
        let up_mon = RECT { left: 0, top: -1440, right: 2560, bottom: 0 };
        let up_work = RECT { bottom: -32 - 70, ..up_mon };
        let (at, up) = tip_anchor(2550, RECT { left: 2520, top: -32, right: 2560, bottom: 0 }, 4, true);
        let o = tip_origin(at, 60, 24, up, up_work);
        assert_eq!(o, (2500, -126));
        assert!(inside(o, 60, 24, up_work));
        // Never off the bounds either way.
        assert_eq!(tip_origin(POINT { x: -50, y: 10 }, 60, 24, true, work), (0, 0));
        assert_eq!(tip_origin(POINT { x: 1910, y: 1070 }, 60, 24, false, work), (1860, 1024));
    }

    #[test]
    fn dots_lay_out_and_hit() {
        assert_eq!(dots_width(0), 0.0);
        assert_eq!(dots_width(1), DOT);
        assert_eq!(dots_width(3), 2.0 * PITCH + DOT);
        assert_eq!(dot_x(0, 3, false), DOT / 2.0);
        assert_eq!(dot_x(2, 3, false), DOT / 2.0 + 2.0 * PITCH);
        // Right to left: desktop 1 is the rightmost dot.
        assert_eq!(dot_x(0, 3, true), dot_x(2, 3, false));
        assert_eq!(dot_x(2, 3, true), dot_x(0, 3, false));
        // Each dot's own center hits it, both ways.
        for rtl in [false, true] {
            for i in 0..5 {
                assert_eq!(dot_at(dot_x(i, 5, rtl), 5, rtl), i);
            }
        }
        // Before the first / past the last dot: the end dots.
        assert_eq!(dot_at(-10.0, 4, false), 0);
        assert_eq!(dot_at(500.0, 4, false), 3);
        assert_eq!(dot_at(-10.0, 4, true), 3);
        assert_eq!(dot_at(500.0, 4, true), 0);
        // Halfway between two dots rounds to the second.
        assert_eq!(dot_at(DOT / 2.0 + PITCH * 0.4, 3, false), 0);
        assert_eq!(dot_at(DOT / 2.0 + PITCH * 0.6, 3, false), 1);
        assert_eq!(dot_at(3.0, 0, false), 0);
    }
}

/// Pictures for the design audit (tests only).
#[cfg(test)]
#[path = "desktops_harmony.rs"]
mod harmony;
