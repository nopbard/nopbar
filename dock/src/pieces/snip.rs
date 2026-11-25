//! A piece of the screen pinned to the desktop (the "snip" shortcut): the
//! pure part. The monitor under the pointer is captured once, before
//! anything of ours shows (capture.rs); the overlay (overlay.rs) shows that
//! still picture dimmed, the user drags out a box on it, and the box is cut
//! from the same picture and becomes an image piece at that very spot on
//! the screen, so it looks like the content stayed there.
//!
//! Here: the selection's state machine, the box math (overlay px, which
//! are the monitor's own px; screen px may be negative left of or above
//! the primary display), the dim around the box, the cut and the PNG.

use windows::Win32::Foundation::RECT;

/// A box smaller than this on either side (px) is a click, not a selection.
pub const MIN_PX: i32 = 8;
/// The dim over everything outside the box (black, this opaque).
pub const DIM_ALPHA: f32 = 0.45;
/// The box's edge (white, this opaque), `line_px` wide.
pub const LINE_ALPHA: f32 = 0.9;

/// A captured picture: opaque BGRA, rows packed top-down.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u8>,
}

/// Where the selection is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum State {
    /// Nothing selected yet.
    Ready,
    /// The button is down: from where it went down to where it is.
    Dragging { from: (i32, i32), to: (i32, i32) },
    /// Chosen: this box (overlay px).
    Done(RECT),
    Cancelled,
}

/// What happens on the overlay (points in overlay px).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ev {
    Down(i32, i32),
    Move(i32, i32),
    Up(i32, i32),
    Enter,
    Esc,
    /// The right button: cancels like Esc.
    Right,
    /// The displays changed, the shortcut was pressed again, the mouse
    /// was taken: cancels (the keyboard goes back if the overlay still has
    /// it).
    Lost,
    /// Another window took the keyboard: cancels, and the keyboard stays
    /// where the user sent it.
    Away,
}

/// Pure: after cancelling with `ev`, does the keyboard go back to the
/// window that had it (the overlay still in front: `front`)? Never when
/// the user went to another window.
pub fn gives_back(ev: Ev, front: bool) -> bool {
    front && ev != Ev::Away
}

/// Pure: the box between two points of a `w`x`h` overlay, ordered and
/// kept inside it (the pointer may leave it while held).
pub fn sel_rect(a: (i32, i32), b: (i32, i32), w: i32, h: i32) -> RECT {
    let cx = |v: i32| v.clamp(0, w.max(0));
    let cy = |v: i32| v.clamp(0, h.max(0));
    let (x0, x1) = (cx(a.0.min(b.0)), cx(a.0.max(b.0)));
    let (y0, y1) = (cy(a.1.min(b.1)), cy(a.1.max(b.1)));
    RECT { left: x0, top: y0, right: x1, bottom: y1 }
}

/// Pure: is `r` big enough to be a selection?
pub fn big_enough(r: &RECT) -> bool {
    r.right - r.left >= MIN_PX && r.bottom - r.top >= MIN_PX
}

/// Pure: the next state on a `w`x`h` overlay. The button going up on a
/// box big enough chooses it (so does Enter while dragging); a click
/// without a drag starts over; Esc, the right button or losing the
/// keyboard cancel. Done and Cancelled stay.
pub fn next(s: State, ev: Ev, w: i32, h: i32) -> State {
    match (s, ev) {
        (State::Done(_) | State::Cancelled, _) => s,
        (_, Ev::Esc | Ev::Right | Ev::Lost | Ev::Away) => State::Cancelled,
        (State::Ready, Ev::Down(x, y)) => State::Dragging { from: (x, y), to: (x, y) },
        (State::Ready, _) => State::Ready,
        (State::Dragging { from, .. }, Ev::Move(x, y)) => State::Dragging { from, to: (x, y) },
        (State::Dragging { from, .. }, Ev::Up(x, y)) => {
            let r = sel_rect(from, (x, y), w, h);
            if big_enough(&r) {
                State::Done(r)
            } else {
                State::Ready
            }
        }
        (State::Dragging { from, to }, Ev::Enter) => {
            let r = sel_rect(from, to, w, h);
            if big_enough(&r) {
                State::Done(r)
            } else {
                s
            }
        }
        // A second press while one is held: the drag goes on.
        (State::Dragging { .. }, Ev::Down(..)) => s,
    }
}

/// Pure: the box shown for a state (None: nothing selected).
pub fn shown(s: State, w: i32, h: i32) -> Option<RECT> {
    match s {
        State::Dragging { from, to } => Some(sel_rect(from, to, w, h)),
        State::Done(r) => Some(r),
        _ => None,
    }
}

/// Pure: the dim around box `sel` on a `w`x`h` overlay: above, below,
/// left of and right of it (the whole overlay above with no box; empty
/// rectangles where the box touches an edge).
pub fn mask(sel: Option<RECT>, w: i32, h: i32) -> [RECT; 4] {
    let r = |l, t, r, b| RECT { left: l, top: t, right: r, bottom: b };
    match sel {
        None => [r(0, 0, w, h), r(0, 0, 0, 0), r(0, 0, 0, 0), r(0, 0, 0, 0)],
        Some(s) => [r(0, 0, w, s.top), r(0, s.bottom, w, h), r(0, s.top, s.left, s.bottom), r(s.right, s.top, w, s.bottom)],
    }
}

/// Pure: the box's edge, `line` px wide, drawn just outside it (so the
/// picture inside stays whole): top, bottom, left, right.
pub fn border(sel: RECT, line: i32) -> [RECT; 4] {
    let l = line.max(1);
    let r = |a, b, c, d| RECT { left: a, top: b, right: c, bottom: d };
    [
        r(sel.left - l, sel.top - l, sel.right + l, sel.top),
        r(sel.left - l, sel.bottom, sel.right + l, sel.bottom + l),
        r(sel.left - l, sel.top, sel.left, sel.bottom),
        r(sel.right, sel.top, sel.right + l, sel.bottom),
    ]
}

/// Pure: the edge's width for a display at `scale`: one DIP, whole px.
pub fn line_px(scale: f32) -> i32 {
    if scale.is_finite() { scale.round().max(1.0) as i32 } else { 1 }
}

/// Pure: overlay box `r` on screen, the overlay's top left at `origin`
/// (the monitor's, px; negative left of or above the primary display).
pub fn to_screen(r: RECT, origin: (i32, i32)) -> RECT {
    RECT { left: r.left + origin.0, top: r.top + origin.1, right: r.right + origin.0, bottom: r.bottom + origin.1 }
}

/// Pure: box `r` of `f` (kept inside it). None when nothing is left.
pub fn crop(f: &Frame, r: RECT) -> Option<Frame> {
    let l = r.left.clamp(0, f.w as i32) as u32;
    let t = r.top.clamp(0, f.h as i32) as u32;
    let rr = r.right.clamp(0, f.w as i32) as u32;
    let b = r.bottom.clamp(0, f.h as i32) as u32;
    if rr <= l || b <= t || crate::gfx::bgra_len(f.w, f.h) != Some(f.px.len()) {
        return None;
    }
    let (w, h) = (rr - l, b - t);
    let mut px = Vec::with_capacity(crate::gfx::bgra_len(w, h)?);
    for y in t..b {
        let start = ((y * f.w + l) * 4) as usize;
        px.extend_from_slice(&f.px[start..start + (w * 4) as usize]);
    }
    Some(Frame { w, h, px })
}

/// A chosen box: its picture cut from the frame and where it goes on
/// screen (px). Quick (a copy of its rows): made on the UI thread; its PNG
/// is made on a helper thread (overlay.rs).
#[derive(Clone, Debug, PartialEq)]
pub struct Cut {
    pub frame: Frame,
    pub rect: RECT,
}

/// Box `sel` of the overlay over `frame` (its top left at screen px
/// `origin`): the picture cut from the frame, and its place on screen.
pub fn cut(frame: &Frame, sel: RECT, origin: (i32, i32)) -> Option<Cut> {
    if !big_enough(&sel) {
        return None;
    }
    let c = crop(frame, sel)?;
    let rect = to_screen(RECT { left: sel.left, top: sel.top, right: sel.left + c.w as i32, bottom: sel.top + c.h as i32 }, origin);
    Some(Cut { frame: c, rect })
}

/// Pure: while its PNG is made, the overlay holds the box's picture where
/// it was: the window shrinks to the box (screen px) and its still picture
/// (the whole monitor's) moves by this much so the box's part shows.
pub fn hold(sel: RECT, origin: (i32, i32)) -> (RECT, (f32, f32)) {
    (to_screen(sel, origin), (-sel.left as f32, -sel.top as f32))
}

/// The frame as PNG bytes (24-bit: a screen has no transparency).
pub fn png_of(f: &Frame) -> Option<Vec<u8>> {
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat24bppBGR, GUID_WICPixelFormat32bppBGRA, IWICBitmapFrameEncode,
        IWICImagingFactory, WICBitmapDitherTypeNone, WICBitmapEncoderNoCache, WICBitmapPaletteTypeCustom,
    };
    use windows::Win32::System::Com::{CoCreateInstance, IStream, CLSCTX_INPROC_SERVER, STATFLAG_NONAME, STATSTG, STREAM_SEEK_SET};
    use windows::Win32::UI::Shell::SHCreateMemStream;
    if crate::gfx::bgra_len(f.w, f.h) != Some(f.px.len()) {
        return None;
    }
    unsafe {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let bmp = wic.CreateBitmapFromMemory(f.w, f.h, &GUID_WICPixelFormat32bppBGRA, f.w * 4, &f.px).ok()?;
        let stream: IStream = SHCreateMemStream(None)?;
        let enc = wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null()).ok()?;
        enc.Initialize(&stream, WICBitmapEncoderNoCache).ok()?;
        let mut fe: Option<IWICBitmapFrameEncode> = None;
        enc.CreateNewFrame(&mut fe, std::ptr::null_mut()).ok()?;
        let fe = fe?;
        fe.Initialize(None).ok()?;
        fe.SetSize(f.w, f.h).ok()?;
        let mut fmt = GUID_WICPixelFormat24bppBGR;
        fe.SetPixelFormat(&mut fmt).ok()?;
        let conv = wic.CreateFormatConverter().ok()?;
        conv.Initialize(&bmp, &fmt, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).ok()?;
        fe.WriteSource(&conv, std::ptr::null()).ok()?;
        fe.Commit().ok()?;
        enc.Commit().ok()?;
        let mut st = STATSTG::default();
        stream.Stat(&mut st, STATFLAG_NONAME).ok()?;
        stream.Seek(0, STREAM_SEEK_SET, None).ok()?;
        let mut out = vec![0u8; st.cbSize as usize];
        let mut got = 0u32;
        stream.Read(out.as_mut_ptr() as *mut _, out.len() as u32, Some(&mut got)).ok().ok()?;
        out.truncate(got as usize);
        Some(out)
    }
}

/// Tests: a PNG's size from its first 24 bytes (the IHDR chunk).
#[cfg(test)]
pub(crate) fn png_size(head: &[u8]) -> Option<(u32, u32)> {
    if head.len() < 24 || &head[..8] != b"\x89PNG\r\n\x1a\n" || &head[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes([head[16], head[17], head[18], head[19]]);
    let h = u32::from_be_bytes([head[20], head[21], head[22], head[23]]);
    (w > 0 && h > 0).then_some((w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rc(l: i32, t: i32, r: i32, b: i32) -> RECT {
        RECT { left: l, top: t, right: r, bottom: b }
    }

    #[test]
    fn a_drag_chooses_a_box_esc_cancels() {
        let (w, h) = (1920, 1080);
        let s = next(State::Ready, Ev::Move(5, 5), w, h);
        assert_eq!(s, State::Ready, "moving alone selects nothing");
        let s = next(s, Ev::Down(100, 200), w, h);
        let s = next(s, Ev::Move(300, 260), w, h);
        assert_eq!(shown(s, w, h), Some(rc(100, 200, 300, 260)));
        assert_eq!(next(s, Ev::Up(400, 300), w, h), State::Done(rc(100, 200, 400, 300)), "the button going up chooses");
        // Dragged up and left: the same box, ordered.
        let s = next(next(State::Ready, Ev::Down(400, 300), w, h), Ev::Up(100, 200), w, h);
        assert_eq!(s, State::Done(rc(100, 200, 400, 300)));
        // Enter while dragging chooses what is shown.
        let s = next(next(next(State::Ready, Ev::Down(10, 10), w, h), Ev::Move(60, 90), w, h), Ev::Enter, w, h);
        assert_eq!(s, State::Done(rc(10, 10, 60, 90)));
        // Esc, the right button, losing the keyboard: cancelled, at any point.
        for ev in [Ev::Esc, Ev::Right, Ev::Lost, Ev::Away] {
            assert_eq!(next(State::Ready, ev, w, h), State::Cancelled);
            assert_eq!(next(State::Dragging { from: (1, 1), to: (50, 50) }, ev, w, h), State::Cancelled);
        }
        // Over: nothing changes it.
        assert_eq!(next(State::Cancelled, Ev::Down(1, 1), w, h), State::Cancelled);
        assert_eq!(next(State::Done(rc(0, 0, 9, 9)), Ev::Esc, w, h), State::Done(rc(0, 0, 9, 9)));
    }

    #[test]
    fn only_a_cancel_from_the_overlay_gives_the_keyboard_back() {
        // Esc or the right button with the overlay in front: back to the app.
        assert!(gives_back(Ev::Esc, true) && gives_back(Ev::Right, true));
        // Another app clicked or Alt+Tab: the keyboard stays there.
        assert!(!gives_back(Ev::Away, true) && !gives_back(Ev::Away, false));
        // The overlay no longer in front (something else took it): left alone.
        assert!(!gives_back(Ev::Esc, false) && !gives_back(Ev::Lost, false));
    }

    #[test]
    fn a_click_without_a_drag_starts_over() {
        let (w, h) = (800, 600);
        let s = next(next(State::Ready, Ev::Down(50, 50), w, h), Ev::Up(53, 52), w, h);
        assert_eq!(s, State::Ready);
        // Thin but long (a line of text) is a box; thinner than MIN_PX is not.
        let s = next(next(State::Ready, Ev::Down(0, 100), w, h), Ev::Up(400, 100 + MIN_PX), w, h);
        assert!(matches!(s, State::Done(_)));
        let s = next(next(State::Ready, Ev::Down(0, 100), w, h), Ev::Up(400, 100 + MIN_PX - 1), w, h);
        assert_eq!(s, State::Ready);
        // Enter with nothing big enough yet: still dragging; with nothing at all: ready.
        let d = next(State::Ready, Ev::Down(5, 5), w, h);
        assert_eq!(next(d, Ev::Enter, w, h), d);
        assert_eq!(next(State::Ready, Ev::Enter, w, h), State::Ready);
    }

    #[test]
    fn the_box_stays_inside_the_overlay() {
        // The pointer left the monitor while held (captured).
        assert_eq!(sel_rect((100, 100), (-50, 5000), 1920, 1080), rc(0, 100, 100, 1080));
        assert_eq!(sel_rect((2000, -3), (1900, 10), 1920, 1080), rc(1900, 0, 1920, 10));
    }

    #[test]
    fn boxes_land_where_they_were_on_any_monitor() {
        let r = rc(10, 20, 110, 70);
        // The primary display.
        assert_eq!(to_screen(r, (0, 0)), r);
        // A display left of and above the primary: negative screen px.
        assert_eq!(to_screen(r, (-2560, -1440)), rc(-2550, -1420, -2450, -1370));
        // One on the right at 150 %: overlay px are its own px, no scaling.
        assert_eq!(to_screen(r, (1920, 0)), rc(1930, 20, 2030, 70));
        assert_eq!(line_px(1.0), 1);
        assert_eq!(line_px(1.5), 2);
        assert_eq!(line_px(2.0), 2);
        assert_eq!(line_px(f32::NAN), 1);
    }

    #[test]
    fn the_dim_frames_the_box() {
        let (w, h) = (100, 80);
        assert_eq!(mask(None, w, h)[0], rc(0, 0, w, h), "nothing selected: all dimmed");
        let m = mask(Some(rc(10, 20, 60, 50)), w, h);
        assert_eq!(m, [rc(0, 0, 100, 20), rc(0, 50, 100, 80), rc(0, 20, 10, 50), rc(60, 20, 100, 50)]);
        // Every pixel is dimmed once, or is in the box.
        let area = |r: &RECT| ((r.right - r.left).max(0) * (r.bottom - r.top).max(0)) as i64;
        assert_eq!(m.iter().map(area).sum::<i64>() + 50 * 30, (w * h) as i64);
        // The edge sits outside the box.
        let b = border(rc(10, 20, 60, 50), 2);
        assert_eq!(b[0], rc(8, 18, 62, 20));
        assert_eq!(b[3], rc(60, 20, 62, 50));
        assert!(b.iter().all(|e| e.right <= 10 || e.left >= 60 || e.bottom <= 20 || e.top >= 50), "no edge over the picture");
    }

    fn frame(w: u32, h: u32) -> Frame {
        // Each pixel says where it is: B = x, G = y.
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                px.extend_from_slice(&[x as u8, y as u8, 7, 255]);
            }
        }
        Frame { w, h, px }
    }

    #[test]
    fn the_cut_is_the_box_exactly() {
        let f = frame(40, 30);
        let c = crop(&f, rc(5, 6, 15, 10)).unwrap();
        assert_eq!((c.w, c.h), (10, 4));
        assert_eq!(&c.px[..4], &[5, 6, 7, 255], "its top left");
        let last = c.px.len() - 4;
        assert_eq!(&c.px[last..], &[14, 9, 7, 255], "its bottom right");
        // Past the edge: cut to the frame; outside it: nothing.
        assert_eq!(crop(&f, rc(35, 25, 60, 60)).map(|c| (c.w, c.h)), Some((5, 5)));
        assert!(crop(&f, rc(50, 50, 60, 60)).is_none());
        assert!(crop(&Frame { w: 2, h: 2, px: vec![0; 3] }, rc(0, 0, 2, 2)).is_none(), "a frame that does not add up");
    }

    #[test]
    fn a_box_becomes_a_png_at_its_screen_spot() {
        let _ = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED) };
        let f = frame(64, 48);
        let c = cut(&f, rc(8, 4, 40, 20), (-1280, 200)).expect("cut");
        assert_eq!((c.frame.w, c.frame.h), (32, 16));
        assert_eq!(c.rect, rc(-1272, 204, -1240, 220));
        let png = png_of(&c.frame).expect("png");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(png_size(&png[..24]), Some((32, 16)));
        assert!(cut(&f, rc(8, 4, 10, 6), (0, 0)).is_none(), "too small");
    }

    #[test]
    fn the_box_is_held_where_it_was_while_its_png_is_made() {
        let (r, off) = hold(rc(100, 50, 300, 150), (-1920, -200));
        assert_eq!(r, rc(-1820, -150, -1620, -50), "the window: the box on screen");
        assert_eq!(off, (-100.0, -50.0), "the still picture moved so the box's part shows");
    }
}
