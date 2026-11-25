//! Graphite glass acceptance (docs/design/widgets-2026-11-17/mock4.html,
//! widgets-v4.png): the widgets drawn by their own painters over photo-like
//! wallpapers (day: fog over grass; night: navy), the glass made from that
//! wallpaper the way the app makes it (1/8, blurred, grey). Every state of
//! the mock, at 100 % and 150 %. Offscreen only: the screen is never read.
use super::{
    backdrop, card,
    grid::{Grid, Span},
    widget::Size,
};
use crate::{
    device_battery::model::{Desktop, Device, Kind},
    gfx::promo_kit as kit,
    icons::Icon,
    notes::{inbox::Row, note, widgets},
};
use windows::Win32::{
    Foundation::RECT,
    Graphics::{
        Direct2D::{Common::*, *},
        Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
    },
};

/// One scene (DIP), as the mock's.
// Keep the archived material fixture at the same screen pixels: noise/
// contrast comparisons must not change their sample when grid sizing changes.
const MATERIAL_FIXTURE_INSET: f32 = 8.;
pub const W: f32 = 640.;
pub const H: f32 = 500.;

pub const MEMO: &str = "빌드 전에\n업데이트 파일 확인\n버전 올리고 태그 푸시\n릴리스 노트 확인";
pub const MEMO_EN: &str = "Release checklist\nReview the build\nCheck release notes\nUpdate the changelog";
pub const SHOPPING: &str = "장보기\n두부, 대파\n계란, 원두";

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}
fn hex(v: u32) -> [f32; 3] {
    [((v >> 16) & 255) as f32, ((v >> 8) & 255) as f32, (v & 255) as f32]
}
/// Pure: a CSS gradient's colour at `t` along stops (position, rgb, alpha).
fn stops(t: f32, s: &[(f32, [f32; 3], f32)]) -> ([f32; 3], f32) {
    if t <= s[0].0 {
        return (s[0].1, s[0].2);
    }
    for w in s.windows(2) {
        if t <= w[1].0 {
            let k = (t - w[0].0) / (w[1].0 - w[0].0).max(1e-6);
            return (mix(w[0].1, w[1].1, k), w[0].2 + (w[1].2 - w[0].2) * k);
        }
    }
    let l = s[s.len() - 1];
    (l.1, l.2)
}

thread_local! {
    /// Scenes over a rough photo instead (review 10's measure).
    static ROUGH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A rough photo-like wallpaper: dense grass blades of every brightness
/// (a real photo's detail, which the 1/8 picture turns into blocks).
pub fn rough(w: u32, h: u32, d: f32) -> Icon {
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 / d, y as f32 / d);
            let blade = ((fx * 0.48 + (fy * 0.07).sin() * 3.).sin() * 0.5 + 0.5) * 70.;
            let n = (card::noise(x / (3. * d) as u32, y / (9. * d) as u32) * 0.5 + 0.5) * 60.;
            let v = (40. + blade + n - fy * 0.05).clamp(0., 255.);
            let i = ((y * w + x) * 4) as usize;
            pixels[i] = (v * 0.7) as u8;
            pixels[i + 1] = v as u8;
            pixels[i + 2] = (v * 0.8) as u8;
            pixels[i + 3] = 255;
        }
    }
    Icon { width: w, height: h, pixels }
}

/// The wallpapers of the pictures (mock5.html's `.desk`s).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Wall {
    /// Fog over grass, an orange glow up right.
    Day,
    /// Navy, a violet glow up right.
    Night,
    /// A bright, pale sky (snow): the tint thickens over it.
    Bright,
}

/// A photo-like wallpaper `w` x `h` px at scale `d`: mock5.html's day
/// (`.desk`) or night (`.desk.night`).
pub fn wallpaper(night: bool, w: u32, h: u32, d: f32) -> Icon {
    wall(if night { Wall::Night } else { Wall::Day }, w, h, d)
}

/// Pure: a CSS radial-gradient's (rx, ry radii, centre cx, cy; fractions
/// of the box) distance at (x, y).
#[allow(clippy::too_many_arguments)]
fn radial(x: f32, y: f32, w: f32, h: f32, rx: f32, ry: f32, cx: f32, cy: f32) -> f32 {
    (((x - cx * w) / (rx * w)).powi(2) + ((y - cy * h) / (ry * h)).powi(2)).sqrt()
}

/// Wallpaper `k`, `w` x `h` px at scale `d`.
pub fn wall(k: Wall, w: u32, h: u32, d: f32) -> Icon {
    if ROUGH.with(|r| r.get()) {
        return rough(w, h, d);
    }
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    let (fw, fh) = (w as f32, h as f32);
    let (s97, c97) = (97f32.to_radians().sin(), 97f32.to_radians().cos());
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let v = fy / fh;
            let c = match k {
                Wall::Night => {
                    let base = stops(radial(fx, fy, fw, fh, 0.9, 0.7, 0.3, 0.3), &[(0., hex(0x2f4f86), 1.), (0.45, hex(0x1b2a4a), 1.), (1., hex(0x0c0f17), 1.)]).0;
                    let (g, a) = stops(radial(fx, fy, fw, fh, 0.5, 0.4, 0.7, 0.25), &[(0., hex(0x6f5bd0), 1.), (0.7, hex(0x6f5bd0), 0.)]);
                    mix(base, g, a)
                }
                Wall::Bright => {
                    let base = stops(v, &[(0., hex(0xf4f7f9), 1.), (0.5, hex(0xe2e9ee), 1.), (1., hex(0xc9d3da), 1.)]).0;
                    let (g, a) = stops(radial(fx, fy, fw, fh, 0.6, 0.5, 0.75, 0.3), &[(0., hex(0xfff3df), 1.), (0.7, hex(0xfff3df), 0.)]);
                    mix(base, g, a)
                }
                Wall::Day => {
                    let lin = stops(v, &[(0., hex(0x9fb1b4), 1.), (0.34, hex(0x7f9590), 1.), (0.55, hex(0x4f6a47), 1.), (0.72, hex(0x33502b), 1.), (1., hex(0x1f3019), 1.)]).0;
                    let (rc, ra) = stops(radial(fx, fy, fw, fh, 1.4, 0.6, 0.3, 0.), &[(0., hex(0xb9c6c8), 1.), (0.3, hex(0x8fa2a6), 1.), (0.62, [120., 132., 128.], 0.)]);
                    let mut c = mix(lin, rc, ra);
                    let (g, a) = stops(radial(fx, fy, fw, fh, 0.6, 0.5, 0.75, 0.3), &[(0., hex(0xd9a066), 1.), (0.7, hex(0xd9a066), 0.)]);
                    c = mix(c, g, a);
                    // Blades of grass in the lower part (the mock's ::after).
                    if v >= 0.55 {
                        let (cx, cy) = (fx / d, fy / d);
                        if (cx * s97 - cy * c97).rem_euclid(5.) < 1. {
                            c = mix(c, [255.; 3], 0.04);
                        }
                    }
                    c
                }
            };
            let i = ((y * w + x) * 4) as usize;
            pixels[i] = c[2].round().clamp(0., 255.) as u8;
            pixels[i + 1] = c[1].round().clamp(0., 255.) as u8;
            pixels[i + 2] = c[0].round().clamp(0., 255.) as u8;
            pixels[i + 3] = 255;
        }
    }
    Icon { width: w, height: h, pixels }
}

/// The bar's mica picture of `img` (crate::wallpaper::mica_image: 1/8,
/// blurred, opaque).
pub fn mica(img: &Icon) -> Icon {
    mica_with_radius(img, 4).blurred
}

pub fn mica_with_radius(img: &Icon, radius: usize) -> crate::wallpaper::MicaPicture {
    let (w, h) = ((img.width / 8).max(16), (img.height / 8).max(16));
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let mut s = [0u32; 4];
            let mut n = 0;
            for yy in (y * 8)..((y * 8 + 8).min(img.height)) {
                for xx in (x * 8)..((x * 8 + 8).min(img.width)) {
                    let i = ((yy * img.width + xx) * 4) as usize;
                    for c in 0..4 {
                        s[c] += img.pixels[i + c] as u32;
                    }
                    n += 1;
                }
            }
            let i = ((y * w + x) * 4) as usize;
            for c in 0..4 {
                px[i + c] = (s[c] / n.max(1)) as u8;
            }
        }
    }
    crate::wallpaper::prepare_mica(Icon { width: w, height: h, pixels: px }, radius)
}

/// The scene's grid (the mock's X0, Y0 and Explorer's 76 x 96 cells).
pub fn grid(d: f32) -> Grid {
    Grid { ox: (36. * d).round() as i32, oy: (46. * d).round() as i32, cw: (76. * d).round() as i32, ch: (96. * d).round() as i32, cols: 8, rows: 4, ..Default::default() }
}
pub fn card_at(g: &Grid, c: i32, r: i32, size: Size, d: f32) -> RECT {
    let (w, h) = size.cells();
    g.rect(Span { c, r, w, h }, (MATERIAL_FIXTURE_INSET * d).round() as i32)
}

pub unsafe fn bitmap(dc: &ID2D1DeviceContext, img: &Icon) -> ID2D1Bitmap1 {
    let props = D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: 96.,
        dpiY: 96.,
        ..Default::default()
    };
    dc.CreateBitmap(D2D_SIZE_U { width: img.width, height: img.height }, Some(img.pixels.as_ptr() as *const _), img.width * 4, &props).unwrap()
}

pub fn devices() -> Vec<Device> {
    vec![
        Device { id: "kb".into(), name: kit::say("Wireless Keyboard", "Keyboard").into(), kind: Kind::Keyboard, percent: Some(50), connected: true, ..Default::default() },
        Device { id: "mouse".into(), name: kit::say("Wireless Mouse", "Mouse").into(), kind: Kind::Mouse, percent: Some(95), connected: true, ..Default::default() },
        // Not connected: never shown (owner 0.1.91: dashed rings).
        Device { id: "pad".into(), name: "게임패드".into(), kind: Kind::Gamepad, percent: Some(80), connected: false, ..Default::default() },
    ]
}
pub fn rows() -> Vec<Row> {
    let titles = [
        kit::say("회의 자료 출력", "Review mockups"),
        kit::say("택배 반품 신청", "Reply to feedback"),
        kit::say("세금계산서 확인", "Send the draft"),
    ];
    let mut r: Vec<Row> = titles.into_iter().map(|s| Row::new(s.into(), 0, None, 0, None)).collect();
    r[0].remind = Some(15 * 3600);
    r[2].completed = Some(0);
    r
}

/// Unix seconds of 14:10 today (local): the memo's date reads "오후 2:10".
pub fn today_at(h: u32, m: u32) -> i64 {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    let secs = t.wHour as i64 * 3600 + t.wMinute as i64 * 60 + t.wSecond as i64;
    now - secs + h as i64 * 3600 + m as i64 * 60
}

/// What one scene shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Normal,
    Night,
    Bright,
    Hover,
    Typing,
    Drag,
    Edit,
    Light,
}
impl State {
    pub const ALL: [State; 8] = [State::Normal, State::Night, State::Bright, State::Hover, State::Typing, State::Drag, State::Edit, State::Light];
    pub fn name(self) -> &'static str {
        match self {
            State::Normal => "normal-day",
            State::Night => "normal-night",
            State::Bright => "normal-bright",
            State::Hover => "hover",
            State::Typing => "typing",
            State::Drag => "drag",
            State::Edit => "edit-mode",
            State::Light => "light",
        }
    }
}

/// The cards of the scene: (kind, place) — memo M, inbox M, battery S,
/// memo S, as the mock lays them out.
pub fn cards(d: f32) -> [(card::Kind, RECT, bool); 4] {
    let g = grid(d);
    [
        (card::Kind::Note, card_at(&g, 0, 0, Size::MEDIUM, d), true),
        (card::Kind::Inbox, card_at(&g, 4, 0, Size::MEDIUM, d), false),
        (card::Kind::Battery, card_at(&g, 0, 2, Size::SMALL, d), false),
        (card::Kind::Note, card_at(&g, 2, 2, Size::SMALL, d), false),
    ]
}

/// Draws card `kind` at `r` (scene px) with its own painter: the plate
/// over the wallpaper under it, its content, its buttons at `alpha`.
pub unsafe fn draw_card(dc: &ID2D1DeviceContext, theme: &crate::theme::Theme, kind: card::Kind, big: bool, r: RECT, d: f32, alpha: f32, lift: bool) {
    let stage = kit::stage();
    let gfx = &stage.gfx;
    let (w, h) = ((r.right - r.left) as f32 / d, (r.bottom - r.top) as f32 / d);
    let under = backdrop::under(r);
    match kind {
        card::Kind::Note => {
            let text = if big { kit::say(MEMO, MEMO_EN) } else { kit::say(SHOPPING, "Shopping\nMilk\nApples") };
            let date = if big { kit::say("오후 2:10", "2:10 PM") } else { "" };
            let f = note::Frame { text, date, editing: false, typing: None, alpha, mark: None, under: under.as_ref(), lift, shadow: true };
            note::frame(dc, gfx, theme, &f, w, h).unwrap();
        }
        card::Kind::Inbox => {
            card::plate(dc, theme, w, h, lift, under.as_ref()).unwrap();
            let theme = card::theme_for(theme, under.as_ref());
            let rows = rows();
            let visible: Vec<usize> = (0..rows.len()).collect();
            widgets::inbox_content(dc, gfx, theme, &widgets::Inbox { rows: &rows, remaining: 2, visible: &visible, hover: None, flash: None }, (w, h), alpha).unwrap();
            card::controls(dc, theme, kind, w, alpha).unwrap();
        }
        card::Kind::Battery => {
            card::plate(dc, theme, w, h, lift, under.as_ref()).unwrap();
            let theme = card::theme_for(theme, under.as_ref());
            let devices = devices();
            let list: Vec<&Device> = devices.iter().collect();
            crate::device_battery::paint::content_sized(dc, gfx, theme, &Desktop { size: if w < 200. { Size::SMALL } else { Size::MEDIUM }, ..Default::default() }, &list, (w, h)).unwrap();
            card::controls(dc, theme, kind, w, alpha).unwrap();
        }
    }
}

/// The memo being typed in: the real note window (hidden) with its editor
/// open, drawn offscreen with its editor's background and text.
unsafe fn typed(dc: &ID2D1DeviceContext, theme: &crate::theme::Theme, r: RECT, d: f32) {
    note::set_look(crate::notes::Look::of(theme));
    note::set_test_scale(Some(d));
    let hwnd = note::create("graphite", r, MEMO).expect("a note window");
    note::set_date(hwnd, today_at(14, 10));
    note::set_test_active(hwnd, true);
    let (pw, ph, px) = note::picture(hwnd).expect("a note picture");
    note::destroy(hwnd);
    note::set_test_scale(None);
    let (w, h) = ((r.right - r.left) as f32 / d, (r.bottom - r.top) as f32 / d);
    let under = backdrop::under(r);
    card::plate(dc, theme, w, h, false, under.as_ref()).unwrap();
    let bmp = bitmap(dc, &Icon { width: pw as u32, height: ph as u32, pixels: px });
    dc.DrawBitmap(&bmp, Some(&D2D_RECT_F { left: 0., top: 0., right: w, bottom: h }), 1., D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None, None);
}

/// One scene at scale `d`: premultiplied BGRA, its size in px.
pub fn scene(state: State, d: f32) -> (Vec<u8>, u32, u32) {
    let _lang = crate::i18n::test_lock("ko");
    let stage = kit::stage();
    let light = state == State::Light;
    let theme = kit::theme(light);
    let cv = kit::Canvas::of(stage.gfx.d2d_factory().clone(), W, H, d);
    let wall = wall(match state { State::Night => Wall::Night, State::Bright => Wall::Bright, _ => Wall::Day }, cv.w, cv.h, d);
    let grey = backdrop::grey_of(RECT { left: 0, top: 0, right: cv.w as i32, bottom: cv.h as i32 }, mica(&wall));
    backdrop::set_test(grey);
    unsafe {
        let mut base = windows_numerics::Matrix3x2::identity();
        cv.dc.GetTransform(&mut base);
        cv.dc.SetTransform(&windows_numerics::Matrix3x2::identity());
        let wb = bitmap(&cv.dc, &wall);
        cv.dc.DrawBitmap(&wb, None, 1., D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None, None);
        for (i, (kind, r, big)) in cards(d).into_iter().enumerate() {
            if matches!(state, State::Drag | State::Edit) && i == 0 {
                continue;
            }
            cv.dc.SetTransform(&(crate::companion::render::translate(r.left as f32 / d, r.top as f32 / d) * base));
            match state {
                State::Typing if i == 0 => typed(&cv.dc, &theme, r, d),
                _ => draw_card(&cv.dc, &theme, kind, big, r, d, if state == State::Hover && i == 0 { 1. } else { 0. }, false),
            }
        }
        cv.dc.SetTransform(&base);
        if matches!(state, State::Drag | State::Edit) {
            edit_scene(&cv.dc, &theme, state, d, base, (cv.w, cv.h));
        }
    }
    backdrop::set_test(None);
    cv.finish()
}

/// The scene as a display (its work area: the whole scene).
pub fn monitor(w: u32, h: u32, d: f32) -> crate::monitors::Monitor {
    let r = RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 };
    crate::monitors::Monitor { handle: 0, rect: r, work: r, primary: true, dpi: (96. * d).round() as u32 }
}

/// A drag (crate::desklayer::drag_hint's marks and shadow, the card
/// lifted) or edit mode (the edit overlay's own painter, drawn on its own
/// layer over the cards as on screen).
unsafe fn edit_scene(dc: &ID2D1DeviceContext, theme: &crate::theme::Theme, state: State, d: f32, base: windows_numerics::Matrix3x2, size: (u32, u32)) {
    let all = cards(d);
    let (kind, home, big) = all[0];
    let m = monitor(size.0, size.1, d);
    let g = grid(d);
    let others: Vec<RECT> = all[1..].iter().map(|c| c.1).collect();
    let inset = (MATERIAL_FIXTURE_INSET * d).round() as i32;
    if state == State::Drag {
        // As the mock: picked up from the top left, carried down right.
        let (dx, dy) = ((4. * 76. + 18.) * d, (2. * 96. + 14.) * d);
        let at = RECT { left: home.left + dx as i32, top: home.top + dy as i32, right: home.right + dx as i32, bottom: home.bottom + dy as i32 };
        let target = super::widget::nearest_free(&g, at, Size::MEDIUM.cells(), inset, &others);
        let slots = super::widget::free_slots(&g, inset, &others);
        dc.SetTransform(&base);
        super::edit_view::marks(dc, m, target, &slots).unwrap();
        let (w, h) = ((at.right - at.left) as f32 / d, (at.bottom - at.top) as f32 / d);
        let k = super::edit_view::LIFT;
        let r = crate::companion::chat::paint::rect(at.left as f32 / d - w * (k - 1.) / 2., at.top as f32 / d - h * (k - 1.) / 2., w * k, h * k);
        crate::gfx::soft_shadows(dc, r, card::RADIUS * k, &card::shadows(false, true)).unwrap();
        dc.SetTransform(&(crate::companion::render::translate(at.left as f32 / d, at.top as f32 / d) * base));
        draw_card(dc, theme, kind, big, at, d, 0., true);
        dc.SetTransform(&base);
        return;
    }
    dc.SetTransform(&(crate::companion::render::translate(home.left as f32 / d, home.top as f32 / d) * base));
    draw_card(dc, theme, kind, big, home, d, 0., false);
    dc.SetTransform(&base);
    // The edit overlay, on its own layer.
    let stage = kit::stage();
    let ov = kit::Canvas::of(stage.gfx.d2d_factory().clone(), W, H, d);
    let frames: Vec<(isize, RECT)> = all.iter().enumerate().map(|(i, c)| (i as isize + 1, c.1)).collect();
    let preview = super::edit::Preview::Note(String::new(), String::new());
    let scene = super::edit_view::Scene { monitor: m, frames: &frames, slots: &[], ghost: None, lift: None, gallery: false, kind: super::edit::Kind::Note, size: Size::SMALL, preview: &preview, preview_hwnd: Default::default(), notes: &[], selected: None, chosen: None, quiet: false, places: &[] };
    super::edit_view::paint(&ov.dc, &stage.gfx, theme, &scene).unwrap();
    let (px, w, h) = ov.finish();
    let bmp = bitmap(dc, &Icon { width: w, height: h, pixels: px });
    dc.SetTransform(&windows_numerics::Matrix3x2::identity());
    dc.DrawBitmap(&bmp, None, 1., D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None, None);
    dc.SetTransform(&base);
}

/// The mock's sheet: the states two by two, 16 DIP apart.
fn sheet(d: f32, shots: &[(State, Vec<u8>, u32, u32)]) -> (Vec<u8>, u32, u32) {
    let gap = (16. * d).round() as u32;
    let (pw, ph) = (shots[0].2, shots[0].3);
    let cols = 2;
    let rows = shots.len().div_ceil(cols) as u32;
    let (w, h) = (cols as u32 * pw + (cols as u32 + 1) * gap, rows * ph + (rows + 1) * gap);
    let mut px = vec![0u8; (w * h * 4) as usize];
    for p in px.chunks_exact_mut(4) {
        p.copy_from_slice(&[13, 13, 13, 255]);
    }
    for (k, (_, s, sw, sh)) in shots.iter().enumerate() {
        let (cx, cy) = ((k % cols) as u32, (k / cols) as u32);
        let (ox, oy) = (gap + cx * (pw + gap), gap + cy * (ph + gap));
        for y in 0..*sh {
            let from = (y * sw * 4) as usize;
            let to = (((oy + y) * w + ox) * 4) as usize;
            px[to..to + (*sw * 4) as usize].copy_from_slice(&s[from..from + (*sw * 4) as usize]);
        }
    }
    (px, w, h)
}

/// cargo test -p dock desklayer::graphite_snapshots -- --ignored
#[test]
#[ignore = "graphite glass PNGs over photo-like wallpapers: target/ui-snapshots/widget-graphite"]
fn widget_graphite() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/ui-snapshots/widget-graphite");
    std::fs::create_dir_all(&root).unwrap();
    for d in [1., 1.5, 2.] {
        let mut shots = Vec::new();
        for state in State::ALL {
            let (px, w, h) = scene(state, d);
            kit::save_png(&px, w, h, &root.join(format!("{}-{}.png", state.name(), (d * 100.) as u32)));
            shots.push((state, px, w, h));
        }
        // Typing over the card at rest, 50 % each: the title and body
        // lines must lie on each other (review: typing looked different).
        let at = |st: State| shots.iter().find(|x| x.0 == st).unwrap();
        let (n, t) = (at(State::Normal), at(State::Typing));
        let mix: Vec<u8> = n.1.iter().zip(&t.1).map(|(a, b)| ((*a as u16 + *b as u16) / 2) as u8).collect();
        let c = cards(d)[0].1;
        let pad = (8. * d) as i32;
        let (x0, y0, x1, y1) = ((c.left - pad).max(0), (c.top - pad).max(0), (c.right + pad).min(n.2 as i32), (c.bottom + pad).min(n.3 as i32));
        let (cw, ch) = ((x1 - x0) as u32, (y1 - y0) as u32);
        let mut crop = Vec::with_capacity((cw * ch * 4) as usize);
        for y in y0..y1 {
            let i = ((y as u32 * n.2 + x0 as u32) * 4) as usize;
            crop.extend_from_slice(&mix[i..i + (cw * 4) as usize]);
        }
        kit::save_png(&crop, cw, ch, &root.join(format!("typing-overlay-{}.png", (d * 100.) as u32)));
        // The mock's order: day, night / hover, typing / drag, light.
        let order = [State::Normal, State::Night, State::Bright, State::Light, State::Hover, State::Typing, State::Drag, State::Edit];
        let ordered: Vec<_> = order.iter().filter_map(|s| shots.iter().position(|x| x.0 == *s)).map(|i| shots[i].clone()).collect();
        let (px, w, h) = sheet(d, &ordered);
        kit::save_png(&px, w, h, &root.join(format!("sheet-{}.png", (d * 100.) as u32)));
    }
    println!("graphite PNGs: {}", root.display());
}

/// A plain part of the small memo over the grass (scene px): under its
/// body text, inside its edge light.
fn plain_part(d: f32) -> RECT {
    let r = cards(d)[3].1;
    RECT { left: r.left + (20. * d) as i32, top: r.top + (124. * d) as i32, right: r.right - (20. * d) as i32, bottom: r.bottom - (14. * d) as i32 }
}

/// (the largest step between neighbouring pixels, the strength of a
/// pattern repeating every 8 px across or down) over `r` of a picture.
fn roughness(px: &(Vec<u8>, u32, u32), r: RECT) -> (i32, f32) {
    let lum = |x: i32, y: i32| {
        let i = ((y as u32 * px.1 + x as u32) * 4) as usize;
        (px.0[i] as i32 + px.0[i + 1] as i32 + px.0[i + 2] as i32) / 3
    };
    let mut step = 0;
    for y in r.top..r.bottom - 1 {
        for x in r.left..r.right - 1 {
            step = step.max((lum(x + 1, y) - lum(x, y)).abs()).max((lum(x, y + 1) - lum(x, y)).abs());
        }
    }
    // Means along each column and each row (the noise averages out), their
    // trend taken away, then the 1/8 px frequency.
    let period = |p: &[f32]| {
        let n = p.len() as f32;
        let slope = (p[p.len() - 1] - p[0]) / (n - 1.);
        let (mut re, mut im) = (0f32, 0f32);
        for (i, v) in p.iter().enumerate() {
            let v = v - (p[0] + slope * i as f32);
            let a = std::f32::consts::TAU * i as f32 / 8.;
            re += v * a.cos();
            im += v * a.sin();
        }
        (re * re + im * im).sqrt() * 2. / n
    };
    let cols: Vec<f32> = (r.left..r.right).map(|x| (r.top..r.bottom).map(|y| lum(x, y) as f32).sum::<f32>() / (r.bottom - r.top) as f32).collect();
    let rows: Vec<f32> = (r.top..r.bottom).map(|y| (r.left..r.right).map(|x| lum(x, y) as f32).sum::<f32>() / (r.right - r.left) as f32).collect();
    (step, period(&cols).max(period(&rows)))
}

/// The longest run of one same brightness along a row over `r` (px): a
/// smooth dark gradient on 8 bits shows as wide flat bands.
fn band(px: &(Vec<u8>, u32, u32), r: RECT) -> i32 {
    let mut longest = 0;
    for y in r.top..r.bottom {
        let mut run = 1;
        let mut last = -1;
        for x in r.left..r.right {
            let i = ((y as u32 * px.1 + x as u32) * 4) as usize;
            let v = (px.0[i] as i32 + px.0[i + 1] as i32 + px.0[i + 2] as i32) / 3;
            run = if v == last { run + 1 } else { 1 };
            last = v;
            longest = longest.max(run);
        }
    }
    longest
}

/// Review 10 ("왜 이렇게 픽셀이 보이냐"): the glass over rough grass
/// shows no blocks, no stripes and no steps: neighbouring pixels differ by
/// the noise at most, nothing repeats every 8 px (the 1/8 picture's
/// pixels), no flat band is wider than 12 px. The old glass shows wide
/// bands.
#[test]
fn graphite_glass_is_smooth() {
    ROUGH.with(|r| r.set(true));
    for d in [1., 2.] {
        let part = plain_part(d);
        let now = scene(State::Normal, d);
        let (step, block) = roughness(&now, part);
        let flat = band(&now, part);
        assert!(step <= 7, "{d}: neighbouring pixels differ by {step}");
        assert!(block < 0.35, "{d}: an 8 px pattern of {block}");
        assert!(flat <= 12, "{d}: a flat band {flat} px wide");
    }
    ROUGH.with(|r| r.set(false));
}

/// The ink box (left, top, right, bottom) brighter than `level` in `band`.
fn ink(px: &(Vec<u8>, u32, u32), band: RECT, level: i32) -> (i32, i32, i32, i32) {
    let (mut l, mut t, mut r, mut b) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for y in band.top..band.bottom {
        for x in band.left..band.right {
            let i = ((y as u32 * px.1 + x as u32) * 4) as usize;
            if (px.0[i] as i32 + px.0[i + 1] as i32 + px.0[i + 2] as i32) / 3 > level {
                l = l.min(x);
                r = r.max(x);
                t = t.min(y);
                b = b.max(y);
            }
        }
    }
    (l, t, r, b)
}

/// The title's and the first body line's ink at rest and while typing.
fn title_and_body(d: f32) -> [((i32, i32, i32, i32), (i32, i32, i32, i32)); 2] {
    let c = cards(d)[0].1;
    let px = |v: f32| (v * d) as i32;
    let title = RECT { left: c.left + px(12.), top: c.top + px(40.), right: c.right - px(12.), bottom: c.top + px(72.) };
    let body = RECT { left: c.left + px(12.), top: c.top + px(74.), right: c.right - px(12.), bottom: c.top + px(94.) };
    let (rest, typing) = (scene(State::Normal, d), scene(State::Typing, d));
    [(ink(&rest, title, 190), ink(&typing, title, 190)), (ink(&rest, body, 160), ink(&typing, body, 160))]
}

/// Review: the title must not move when typing starts. Its ink at rest
/// and in the editor lies within 1 px (left, right, top, bottom); the
/// body's first line too.
#[test]
fn typing_title_lies_on_the_cards_title() {
    for d in [1., 1.5, 2.] {
        for (what, (a, b)) in ["title", "body"].into_iter().zip(title_and_body(d)) {
            assert!(a.0 < a.2, "{d}: a {what} at rest");
            // Where it starts and its top and foot (its baseline) within
            // 1 px; its end within 2 (GDI sets each glyph on whole pixels).
            let worst = [a.0 - b.0, a.1 - b.1, a.3 - b.3].iter().map(|v| v.abs()).max().unwrap();
            assert!(worst <= 1 && (a.2 - b.2).abs() <= 2, "{d} {what}: rest {a:?} typing {b:?}");
        }
    }
}
