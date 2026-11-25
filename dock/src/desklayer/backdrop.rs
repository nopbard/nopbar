//! What the widgets' glass shows: the wallpaper, blurred once, its colour
//! kept (saturate 1.3, glass C).
//!
//! The bar's mica picture (crate::wallpaper::mica_picture: the wallpaper of
//! a display, shrunk to 1/8 and strongly blurred: no shapes, only the flow
//! of colour and light) is made once per display, wallpaper and scale on a
//! thread of its own, saturated, laid out smoothly at the display's own
//! pixels (a cubic B-spline), and kept: one picture per display. A card
//! cuts the piece under its own place (screen px) out of it, then applies
//! its tint and the cached dither (card::body). Static wallpapers only: live wallpapers use
//! live::Layer and never request this picture. A moved static card only
//! cuts another piece, right away.
//!
//! A new wallpaper or a display change (crate::wallpaper::invalidate)
//! makes the next request take a new picture; the cards that drew one are
//! told (WM_BACKDROP) when it is ready. Without a picture (none, a plain
//! colour, unreadable) the cards are a solid colour (card::body).
use crate::icons::Icon;
use crate::theme::{luminance, AdaptiveGlass, Rgba};
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted to a card window when the picture under it changed.
pub const WM_BACKDROP: u32 = WM_APP + 0x2c3;

/// The saturation of the C glass.
pub const SATURATE: f32 = AdaptiveGlass::SATURATION;
/// The noise over the picture (± of 255, fixed per screen pixel).
pub const DITHER: f32 = 0.02 * 255. / 2.;

/// A display's blurred wallpaper at its own pixels.
pub struct Grey {
    /// The display (screen px) the picture covers.
    pub mon: RECT,
    /// The picture, one px per screen px of `mon`. BGR holds the wallpaper
    /// and the fourth byte holds the screen-anchored dither.
    pub img: Icon,
    /// Blurred mica at its source resolution, for Composition sampling.
    pub coarse: Icon,
    /// Summed relative luminance, one row and column larger than the
    /// small picture. A card's mean luminance takes four reads.
    sums: Vec<f64>,
    /// The small picture's size (its px are `mon` / this).
    small: (u32, u32),
}

/// What lies under one card: the picture of its display and where the
/// card is (screen px).
#[derive(Clone)]
pub struct Under {
    pub grey: Arc<Grey>,
    pub card: RECT,
    pub glass: AdaptiveGlass,
}

impl Under {
    fn new(grey: Arc<Grey>, card: RECT) -> Self {
        let glass = AdaptiveGlass::new(grey.luminance(card), None);
        Self { grey, card, glass }
    }
}

impl Grey {
    /// The mean relative luminance of the piece over `r` (screen px).
    pub fn luminance(&self, r: RECT) -> f32 {
        let (sw, sh) = self.small;
        let (mw, mh) = ((self.mon.right - self.mon.left).max(1) as f32, (self.mon.bottom - self.mon.top).max(1) as f32);
        let to = |v: i32, o: i32, m: f32, n: u32| (((v - o) as f32 / m * n as f32).round() as i64).clamp(0, n as i64) as usize;
        let (mut x0, mut x1) = (to(r.left, self.mon.left, mw, sw), to(r.right, self.mon.left, mw, sw));
        let (mut y0, mut y1) = (to(r.top, self.mon.top, mh, sh), to(r.bottom, self.mon.top, mh, sh));
        // At least one small px (a tiny card, a card off the display).
        if x1 <= x0 {
            x0 = x0.min(sw as usize - 1);
            x1 = x0 + 1;
        }
        if y1 <= y0 {
            y0 = y0.min(sh as usize - 1);
            y1 = y0 + 1;
        }
        let row = sw as usize + 1;
        let at = |x: usize, y: usize| self.sums[y * row + x];
        let (a, b, c, d) = (at(x1, y1), at(x0, y1), at(x1, y0), at(x0, y0));
        ((a - b - c + d) / ((x1 - x0) * (y1 - y0)) as f64) as f32
    }

    /// The picture's BGR and dither under `r` (screen px), `r`'s size.
    /// Beyond the display, its edge goes on.
    pub fn cut(&self, r: RECT) -> (i32, i32, Vec<u8>) {
        let (w, h) = ((r.right - r.left).max(0), (r.bottom - r.top).max(0));
        let (iw, ih) = (self.img.width as i32, self.img.height as i32);
        let mut out = vec![0u8; (w * h * 4) as usize];
        if iw == 0 || ih == 0 {
            return (w, h, out);
        }
        let x0 = r.left - self.mon.left;
        // The run of the row inside the picture, copied whole.
        let (inside0, inside1) = (x0.clamp(0, iw), (x0 + w).clamp(0, iw));
        for y in 0..h {
            let sy = (r.top - self.mon.top + y).clamp(0, ih - 1);
            let src = &self.img.pixels[(sy * iw * 4) as usize..((sy + 1) * iw * 4) as usize];
            let dst = &mut out[(y * w * 4) as usize..((y + 1) * w * 4) as usize];
            if inside1 > inside0 {
                let from = (inside0 - x0) as usize * 4;
                dst[from..from + ((inside1 - inside0) * 4) as usize].copy_from_slice(&src[(inside0 * 4) as usize..(inside1 * 4) as usize]);
            }
            for x in 0..w {
                let sx = x0 + x;
                if sx < 0 || sx >= iw {
                    let sx = sx.clamp(0, iw - 1) as usize;
                    dst[x as usize * 4..x as usize * 4 + 4].copy_from_slice(&src[sx * 4..sx * 4 + 4]);
                }
            }
        }
        (w, h, out)
    }

    /// Bytes kept for this picture.
    pub fn bytes(&self) -> usize {
        self.img.pixels.len() + self.sums.len() * std::mem::size_of::<f64>()
    }
}

#[cfg_attr(test, allow(dead_code))]
struct Kept {
    grey: Option<Arc<Grey>>,
    mon: RECT,
    dpi: u32,
    generation: u64,
}

struct State {
    kept: Vec<Kept>,
    /// Displays whose picture is being made.
    making: Vec<RECT>,
    /// Windows to tell when a picture is ready and their last glass polarity.
    listeners: Vec<(isize, Option<AdaptiveGlass>)>,
}

static STATE: Mutex<State> = Mutex::new(State { kept: Vec::new(), making: Vec::new(), listeners: Vec::new() });

#[cfg(test)]
thread_local! {
    /// Pictures' own wallpaper (tests never read the real one).
    static TEST: std::cell::RefCell<Option<Arc<Grey>>> = const { std::cell::RefCell::new(None) };
}

/// Pictures draw over `grey` (tests only).
#[cfg(test)]
pub fn set_test(grey: Option<Arc<Grey>>) {
    TEST.with(|t| *t.borrow_mut() = grey);
}

/// Pure: `img` (premultiplied opaque BGRA) saturated by `s` (CSS
/// saturate(): the luma kept, the colour pushed away from it).
pub fn saturated(img: &Icon, s: f32) -> Icon {
    let mut pixels = img.pixels.clone();
    let m = [
        [0.213 + 0.787 * s, 0.715 - 0.715 * s, 0.072 - 0.072 * s],
        [0.213 - 0.213 * s, 0.715 + 0.285 * s, 0.072 - 0.072 * s],
        [0.213 - 0.213 * s, 0.715 - 0.715 * s, 0.072 + 0.928 * s],
    ];
    for p in pixels.as_chunks_mut::<4>().0 {
        let (r, g, b) = (p[2] as f32, p[1] as f32, p[0] as f32);
        let c = |k: usize| (m[k][0] * r + m[k][1] * g + m[k][2] * b).round().clamp(0., 255.) as u8;
        let (nr, ng, nb) = (c(0), c(1), c(2));
        p[2] = nr;
        p[1] = ng;
        p[0] = nb;
    }
    Icon { width: img.width, height: img.height, pixels }
}

/// Pure: the cubic B-spline weights of the four neighbours at fraction `t`
/// (smooth through its second derivative: no seams between the small px).
fn bspline(t: f32) -> [f32; 4] {
    let t2 = t * t;
    let t3 = t2 * t;
    [(1. - t).powi(3) / 6., (3. * t3 - 6. * t2 + 4.) / 6., (-3. * t3 + 3. * t2 + 3. * t + 1.) / 6., t3 / 6.]
}

/// Pure: for each of `n` output px, the first of its four source px (in
/// `0..m`, clamped at the edges) and their weights.
fn taps(n: u32, m: u32) -> Vec<([usize; 4], [f32; 4])> {
    let k = m as f32 / n.max(1) as f32;
    (0..n)
        .map(|i| {
            let u = (i as f32 + 0.5) * k - 0.5;
            let f = u.floor();
            let w = bspline(u - f);
            let at = |d: i32| (f as i32 + d).clamp(0, m as i32 - 1) as usize;
            ([at(-1), at(0), at(1), at(2)], w)
        })
        .collect()
}

/// The small picture laid out at display size with one dither sample per pixel.
fn upscale(img: &Icon, w: u32, h: u32, origin: (i32, i32)) -> Icon {
    let (sw, sh) = (img.width, img.height);
    let tx = taps(w, sw);
    let ty = taps(h, sh);
    // Across first: each small row at the full width.
    let mut rows = vec![0f32; (w * sh * 3) as usize];
    for y in 0..sh as usize {
        let src = &img.pixels[y * sw as usize * 4..(y + 1) * sw as usize * 4];
        let dst = &mut rows[y * w as usize * 3..(y + 1) * w as usize * 3];
        for (x, (at, wt)) in tx.iter().enumerate() {
            for c in 0..3 {
                dst[x * 3 + c] = (0..4).map(|k| src[at[k] * 4 + c] as f32 * wt[k]).sum();
            }
        }
    }
    let mut pixels = vec![255u8; (w * h * 4) as usize];
    for (y, (at, wt)) in ty.iter().enumerate() {
        let r: [&[f32]; 4] = std::array::from_fn(|k| &rows[at[k] * w as usize * 3..(at[k] + 1) * w as usize * 3]);
        let dst = &mut pixels[y * w as usize * 4..(y + 1) * w as usize * 4];
        for x in 0..w as usize {
            for c in 0..3 {
                let v = r[0][x * 3 + c] * wt[0] + r[1][x * 3 + c] * wt[1] + r[2][x * 3 + c] * wt[2] + r[3][x * 3 + c] * wt[3];
                dst[x * 4 + c] = v.round().clamp(0., 255.) as u8;
            }
            let noise = super::card::noise((origin.0 + x as i32) as u32, (origin.1 + y as i32) as u32);
            dst[x * 4 + 3] = ((noise + 1.) * 127.5).round() as u8;
        }
    }
    Icon { width: w, height: h, pixels }
}

/// Pure: the summed-area table of `img`'s relative luminance.
fn sums(img: &Icon) -> Vec<f64> {
    let (w, h) = (img.width as usize, img.height as usize);
    let row = w + 1;
    let mut s = vec![0.; row * (h + 1)];
    for y in 0..h {
        let mut run = 0.;
        for x in 0..w {
            let i = (y * w + x) * 4;
            let p = &img.pixels[i..i + 4];
            run += f64::from(luminance(Rgba { r: p[2], g: p[1], b: p[0], a: 255 }));
            s[(y + 1) * row + x + 1] = s[y * row + x + 1] + run;
        }
    }
    s
}

/// Pure: measure the source pixels and display the blurred pixels.
fn make(mon: RECT, source: &Icon, blurred: &Icon) -> Option<Grey> {
    let (w, h) = (source.width, source.height);
    if w == 0 || h == 0 || (blurred.width, blurred.height) != (w, h)
        || source.pixels.len().min(blurred.pixels.len()) < (w * h * 4) as usize {
        return None;
    }
    let small = saturated(blurred, SATURATE);
    let (mw, mh) = ((mon.right - mon.left).max(1) as u32, (mon.bottom - mon.top).max(1) as u32);
    let big = upscale(&small, mw, mh, (mon.left, mon.top));
    Some(Grey { mon, sums: sums(source), small: (w, h), img: big, coarse: small })
}

pub fn grey_of_mica(mon: RECT, picture: &crate::wallpaper::MicaPicture) -> Option<Arc<Grey>> {
    make(mon, &picture.source, &picture.blurred).map(Arc::new)
}

/// The display holding most of `r` (its full rectangle and dpi).
#[cfg_attr(test, allow(dead_code))]
fn display_of(r: RECT) -> Option<(RECT, u32)> {
    let mons = crate::monitors::all();
    let i = super::grid::monitor_of(r, &mons)?;
    Some((mons[i].rect, mons[i].dpi))
}

/// Window `hwnd` draws a card on glass: told when its picture changes.
pub fn listen(hwnd: HWND) {
    if let Ok(mut s) = STATE.lock() {
        let h = hwnd.0 as isize;
        if !s.listeners.iter().any(|&(hwnd, _)| hwnd == h) {
            s.listeners.push((h, None));
        }
    }
}

/// Window `hwnd` is told when the picture changes (tests).
#[cfg(test)]
pub fn listening(hwnd: HWND) -> bool {
    STATE.lock().is_ok_and(|s| s.listeners.iter().any(|&(h, _)| h == hwnd.0 as isize))
}

pub fn unlisten(hwnd: HWND) {
    if let Ok(mut s) = STATE.lock() {
        s.listeners.retain(|&(h, _)| h != hwnd.0 as isize);
    }
}

fn tell() {
    let all = STATE.lock().map(|s| s.listeners.clone()).unwrap_or_default();
    for (h, _) in all {
        unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), WM_BACKDROP, WPARAM(0), LPARAM(0));
        }
    }
}

/// The wallpaper may have changed (crate::wallpaper::invalidate): the
/// cards draw again, asking for a new picture (the old one shows till the
/// new one is ready).
pub fn changed() {
    tell();
}

/// What lies under card `card` (screen px): None (a solid card) while
/// there is no picture of its display yet; one is asked for then.
pub fn under(card: RECT) -> Option<Under> {
    picture_at(card).map(|grey| Under::new(grey, card))
}

fn picture_at(card: RECT) -> Option<Arc<Grey>> {
    if super::live::enabled() { return None; }
    #[cfg(test)]
    {
        let _ = card;
        TEST.with(|t| t.borrow().clone())
    }
    #[cfg(not(test))]
    {
        let (mon, dpi) = display_of(card)?;
        picture(mon, dpi)
    }
}

/// A named card keeps its previous polarity as it moves and the wallpaper changes.
pub fn under_for(hwnd: HWND, card: RECT) -> Option<Under> {
    let mut under = under(card)?;
    if let Ok(mut s) = STATE.lock() {
        if let Some((_, previous)) = s.listeners.iter_mut().find(|(h, _)| *h == hwnd.0 as isize) {
            under.glass = AdaptiveGlass::new(under.grey.luminance(card), previous.map(|glass| glass.light));
            *previous = Some(under.glass);
        }
    }
    Some(under)
}

/// Keep the last painted tone and tint for the whole drag.
pub(super) fn for_drag(hwnd: HWND, card: RECT) -> Option<Under> {
    let grey = picture_at(card)?;
    let style = STATE.lock().ok().and_then(|s| {
        s.listeners.iter().find(|(h, _)| *h == hwnd.0 as isize).and_then(|(_, glass)| *glass)
    });
    Some(match style {
        Some(glass) => Under { grey, card, glass },
        None => Under::new(grey, card),
    })
}

/// What lies under card `card` (screen px) on display `m`, whichever
/// display the card is mostly on: a card across two displays shows each
/// display's own picture on that display (crate::desklayer::drag_hint).
pub fn under_on(card: RECT, m: crate::monitors::Monitor) -> Option<Under> {
    picture_on(m).map(|grey| Under::new(grey, card))
}

pub(super) fn picture_on(m: crate::monitors::Monitor) -> Option<Arc<Grey>> {
    if super::live::enabled() { return None; }
    #[cfg(test)]
    {
        let _ = m;
        TEST.with(|t| t.borrow().clone())
    }
    #[cfg(not(test))]
    {
        picture(m.rect, m.dpi)
    }
}

/// The picture of display `mon` (at `dpi`) under `card`; asked for when
/// there is none of this wallpaper and DPI yet.
#[cfg(not(test))]
fn picture(mon: RECT, dpi: u32) -> Option<Arc<Grey>> {
    {
        let generation = crate::wallpaper::generation();
        let mut s = STATE.lock().ok()?;
        let at = s.kept.iter().position(|k| k.mon == mon);
        let fresh = at.is_some_and(|i| s.kept[i].generation == generation && s.kept[i].dpi == dpi);
        let grey = at.and_then(|i| s.kept[i].grey.clone());
        if !fresh && !s.making.contains(&mon) {
            s.making.push(mon);
            drop(s);
            ask(mon, dpi, generation);
        }
        grey
    }
}

/// Makes the picture of display `mon` on a thread of its own (the bar's
/// mica image needs COM and may read the desktop for up to its timeout).
#[cfg_attr(test, allow(dead_code))]
fn ask(mon: RECT, dpi: u32, generation: u64) {
    let spawned = std::thread::Builder::new().name("glass-backdrop".into()).spawn(move || {
        use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
        let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
        let grey = crate::wallpaper::mica_picture(mon).and_then(|picture| grey_of_mica(mon, &picture));
        if com {
            unsafe { CoUninitialize() };
        }
        if let Ok(mut s) = STATE.lock() {
            s.making.retain(|m| *m != mon);
            s.kept.retain(|k| k.mon != mon);
            // Only the displays there now (one picture each).
            let now: Vec<RECT> = crate::monitors::all().iter().map(|m| m.rect).collect();
            s.kept.retain(|k| now.contains(&k.mon));
            s.kept.push(Kept { grey, mon, dpi, generation });
            let total: usize = s.kept.iter().filter_map(|k| k.grey.as_ref()).map(|g| g.bytes()).sum();
            crate::log!("widgets: glass picture {}x{} made; {} KB kept for {} display(s)", mon.right - mon.left, mon.bottom - mon.top, total / 1024, s.kept.len());
        }
        tell();
    });
    if spawned.is_err() {
        if let Ok(mut s) = STATE.lock() {
            s.making.retain(|m| *m != mon);
        }
    }
}

/// Tests: a display's picture from `img` (as if made by the bar).
#[cfg(test)]
pub fn grey_of(mon: RECT, img: Icon) -> Option<Arc<Grey>> {
    make(mon, &img, &img).map(Arc::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn img(px: &[[u8; 3]]) -> Icon {
        Icon { width: px.len() as u32, height: 1, pixels: px.iter().flat_map(|c| [c[0], c[1], c[2], 255]).collect() }
    }
    /// The wallpaper's colour stays (saturated 1.3, the luma kept).
    #[test]
    fn the_picture_keeps_the_wallpapers_colour() {
        let mon = RECT { left: 0, top: 0, right: 16, bottom: 8 };
        // Grass green and an orange glow (BGRA).
        let g = grey_of(mon, img(&[[40, 120, 60], [70, 160, 220]])).expect("a picture");
        assert_eq!((g.img.width, g.img.height), (16, 8), "the display's own px");
        // The green half stays green, the orange half orange.
        let px = |x: usize| &g.img.pixels[x * 4..x * 4 + 3];
        let (green, orange) = (px(1), px(14));
        assert!(green[1] > green[0] + 40 && green[1] > green[2] + 40, "{green:?}");
        assert!(orange[2] > orange[0] + 100, "{orange:?}");
        // Saturate 1.3: further from grey than the source, its luma kept.
        let s = saturated(&img(&[[70, 160, 220]]), SATURATE);
        let (r, gg, b) = (s.pixels[2] as f32, s.pixels[1] as f32, s.pixels[0] as f32);
        assert!(r - b > 220. - 70. + 20., "{r} {b}");
        let luma = |r: f32, g: f32, b: f32| 0.213 * r + 0.715 * g + 0.072 * b;
        assert!((luma(r, gg, b) - luma(220., 160., 70.)).abs() < 2.);
        // A genuine flat wallpaper file still gives the glass its polarity.
        let flat = grey_of(mon, img(&[[248, 248, 248]; 2])).unwrap();
        assert!(AdaptiveGlass::new(flat.luminance(mon), None).light);
        // A broken picture: the solid card.
        assert!(grey_of(mon, Icon { width: 4, height: 4, pixels: vec![0; 8] }).is_none());
        let mixed = make(mon, &img(&[[230; 3]; 2]), &img(&[[20; 3]; 2])).unwrap();
        assert!(AdaptiveGlass::new(mixed.luminance(mon), None).light);
        assert_eq!(mixed.img.pixels[0], 20);
    }
    /// Laid out without seams: neighbouring px differ by little more than
    /// the noise, and the means over a region are the small picture's.
    #[test]
    fn the_picture_is_smooth_and_its_means_are_right() {
        let mon = RECT { left: -800, top: 0, right: 0, bottom: 400 };
        let small = Icon { width: 100, height: 50, pixels: (0..100 * 50).flat_map(|i| { let x = (i % 100) as u8; [x * 2, 100, 255 - x * 2, 255] }).collect() };
        let g = grey_of(mon, small).unwrap();
        let w = g.img.width as usize;
        let mut step = 0;
        for y in 0..400usize {
            for x in 0..w - 1 {
                let i = (y * w + x) * 4;
                step = step.max((g.img.pixels[i] as i32 - g.img.pixels[i + 4] as i32).abs());
            }
        }
        assert!(step <= 2 * DITHER.ceil() as i32 + 3, "a step of {step}");
        // Relative luminance weights the left half's red more than the right half's blue.
        let left = g.luminance(RECT { left: -800, top: 0, right: -400, bottom: 400 });
        let right = g.luminance(RECT { left: -400, top: 0, right: 0, bottom: 400 });
        assert!(left > right + 0.05, "{left:?} {right:?}");
        // A cut beyond the display repeats its edge.
        let (cw, ch, px) = g.cut(RECT { left: -810, top: -5, right: -790, bottom: 5 });
        assert_eq!((cw, ch, px.len()), (20, 10, 800));
        assert_eq!(&px[0..4], &g.img.pixels[0..4]);
    }
    /// DONE's measure: one 4K display's picture (kept bytes, time to make
    /// in this build). cargo test -p dock backdrop::tests::four_k -- --ignored --nocapture
    #[test]
    #[ignore = "measure: a 4K display's glass picture"]
    fn four_k() {
        let mon = RECT { left: 0, top: 0, right: 3840, bottom: 2160 };
        let small = Icon { width: 480, height: 270, pixels: (0..480 * 270).flat_map(|i| [(i % 251) as u8, 120, (i % 97) as u8, 255]).collect() };
        let t = std::time::Instant::now();
        let g = grey_of(mon, small).unwrap();
        println!("4K glass picture: {} bytes kept ({:.1} MB), made in {} ms", g.bytes(), g.bytes() as f64 / 1048576., t.elapsed().as_millis());
        assert_eq!(g.img.pixels.len(), 3840 * 2160 * 4);
    }
    #[test]
    fn the_display_of_a_card_is_the_one_holding_most_of_it() {
        // Through the real display list: any card on a display maps to a
        // display rectangle holding its centre.
        if let Some(m) = crate::monitors::all().first().copied() {
            let r = RECT { left: m.rect.left + 10, top: m.rect.top + 10, right: m.rect.left + 110, bottom: m.rect.top + 110 };
            assert_eq!(display_of(r).map(|d| d.0), Some(m.rect));
        }
    }
}
