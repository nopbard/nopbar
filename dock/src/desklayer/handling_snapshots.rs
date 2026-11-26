//! Acceptance pictures for docs/superpowers/briefs/2026-11-17-widget-handling.md
//! (mock: docs/design/widgets-2026-11-17b/widgets-v5.png): the widgets drawn
//! by their own painters over the mock's wallpapers (day, night, a bright
//! one) and the light theme; the long press's handles; every size. Offscreen
//! only: the screen is never read.
use super::{
    backdrop, card, edit_view,
    graphite_snapshots::{self as gs, Wall},
    grid::{Grid, Span},
    widget::{self, Size},
};
use crate::{
    device_battery::model::{Desktop, Device, Kind as DevKind},
    gfx::promo_kit as kit,
    icons::Icon,
    notes::{note, widgets},
};
use windows::Win32::{
    Foundation::RECT,
    Graphics::Direct2D::{Common::D2D1_COMPOSITE_MODE_SOURCE_OVER, ID2D1DeviceContext, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR},
};
use windows_numerics::Matrix3x2;

pub const MEMO_LONG: &str = "빌드 전에\n업데이트 파일 확인\n버전 올리고 태그 푸시\n릴리스 노트 확인\n태그 푸시 후 공지\n체크리스트 정리";

/// One scene's grid: the mock's X0 36, Y0 40, places of 2 x 2 Explorer
/// cells (76 x 96 DIP).
pub fn grid(d: f32, places: (i32, i32)) -> Grid {
    Grid { ox: (36. * d).round() as i32, oy: (40. * d).round() as i32, cw: (76. * d).round() as i32, ch: (96. * d).round() as i32, cols: places.0 * widget::SLOT, rows: places.1 * widget::SLOT, ..Default::default() }
}
/// The card of `size` at place (`pc`, `pr`) (scene px).
pub fn at(g: &Grid, pc: i32, pr: i32, size: Size, d: f32) -> RECT {
    let (w, h) = size.cells();
    g.rect(Span { c: pc * widget::SLOT, r: pr * widget::SLOT, w, h }, (widget::INSET * d).round() as i32)
}

pub fn devices() -> Vec<Device> {
    let mut v = gs::devices();
    v.push(Device { id: "head".into(), name: "Headphones".into(), kind: DevKind::Headphones, percent: Some(72), connected: true, ..Default::default() });
    v.push(Device { id: "pen".into(), name: "Stylus".into(), kind: DevKind::Other, percent: Some(15), connected: true, ..Default::default() });
    v
}

/// What a card shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Show {
    Note(&'static str),
    Inbox,
    Battery,
}

/// Draws card `show` of `size` at `r` (scene px; the transform at its
/// corner) with the widget's own live painter: its shadow (its shade
/// window), its glass, its content, no hover buttons.
pub unsafe fn live(dc: &ID2D1DeviceContext, theme: &crate::theme::Theme, show: Show, size: Size, r: RECT, d: f32) {
    let stage = kit::stage();
    let gfx = &stage.gfx;
    let (w, h) = ((r.right - r.left) as f32 / d, (r.bottom - r.top) as f32 / d);
    let under = backdrop::under(r);
    let body_theme = card::theme_for(theme, under.as_ref());
    match show {
        Show::Note(text) => {
            let f = note::Frame { text, date: kit::say("오후 2:10", "2:10 PM"), editing: false, typing: None, alpha: 0., mark: None, under: under.as_ref(), lift: false, shadow: true };
            note::frame(dc, gfx, theme, &f, w, h).unwrap();
        }
        Show::Inbox => {
            card::plate(dc, theme, w, h, false, under.as_ref()).unwrap();
            let rows = gs::rows();
            let visible: Vec<usize> = (0..rows.len()).collect();
            widgets::inbox_content(dc, gfx, body_theme, &widgets::Inbox { rows: &rows, remaining: 2, visible: &visible, hover: None, flash: None }, (w, h), 0.).unwrap();
            card::controls(dc, body_theme, card::Kind::Inbox, w, 0.).unwrap();
        }
        Show::Battery => {
            card::plate(dc, theme, w, h, false, under.as_ref()).unwrap();
            let devices = devices();
            let list: Vec<&Device> = devices.iter().collect();
            crate::device_battery::paint::content_sized(dc, gfx, body_theme, &Desktop { size, ..Default::default() }, &list, (w, h)).unwrap();
            card::controls(dc, body_theme, card::Kind::Battery, w, 0.).unwrap();
        }
    }
}

/// A canvas `w` x `h` DIP at `d` with wallpaper `wall` and its glass picture.
fn desk(wall: Wall, w: f32, h: f32, d: f32) -> kit::Canvas {
    let stage = kit::stage();
    let cv = kit::Canvas::of(stage.gfx.d2d_factory().clone(), w, h, d);
    let img = gs::wall(wall, cv.w, cv.h, d);
    backdrop::set_test(backdrop::grey_of(RECT { left: 0, top: 0, right: cv.w as i32, bottom: cv.h as i32 }, gs::mica(&img)));
    unsafe {
        let mut base = Matrix3x2::identity();
        cv.dc.GetTransform(&mut base);
        cv.dc.SetTransform(&Matrix3x2::identity());
        let b = gs::bitmap(&cv.dc, &img);
        cv.dc.DrawBitmap(&b, None, 1., D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None, None);
        cv.dc.SetTransform(&base);
    }
    cv
}

/// Draws `cards` (show, size, rect) with their live painters on `cv`.
unsafe fn lay(cv: &kit::Canvas, theme: &crate::theme::Theme, cards: &[(Show, Size, RECT)], d: f32) {
    let mut base = Matrix3x2::identity();
    cv.dc.GetTransform(&mut base);
    for (show, size, r) in cards {
        cv.dc.SetTransform(&(crate::companion::render::translate(r.left as f32 / d, r.top as f32 / d) * base));
        live(&cv.dc, theme, *show, *size, *r, d);
    }
    cv.dc.SetTransform(&base);
}

pub(crate) fn paint_site_widgets(cv: &kit::Canvas, theme: &crate::theme::Theme, d: f32) -> RECT {
    let mut g = grid(d, (4, 2));
    g.ox = cv.w as i32 - g.cw * g.cols - (40.0 * d).round() as i32;
    g.oy = (56.0 * d).round() as i32;
    let memo = "Studio checklist\nReview layout proofs\nPick cover artwork\nCheck the export\nSend the final draft";
    let cards = [
        (Show::Note(memo), Size::LARGE, (0, 0)),
        (Show::Inbox, Size::MEDIUM, (2, 0)),
        (Show::Battery, Size::MEDIUM, (2, 1)),
    ].map(|(show, size, (col, row))| (show, size, at(&g, col, row, size, d)));
    unsafe {
        cv.dc.SetTransform(&crate::companion::render::scale(d, d));
        lay(cv, theme, &cards, d);
    }
    RECT { left: g.ox, top: g.oy, right: g.ox + g.cw * g.cols, bottom: g.oy + g.ch * g.rows }
}

pub(crate) fn render_readme_widgets(path: &std::path::Path) {
    let d = 1.0;
    let cv = desk(Wall::Night, 680.0, 464.0, d);
    let grid = grid(d, (4, 2));
    let theme = kit::theme(false);
    let views = [
        (Show::Note(gs::MEMO_EN), Size::LARGE, (0, 0)),
        (Show::Inbox, Size::MEDIUM, (2, 0)),
        (Show::Battery, Size::MEDIUM, (2, 1)),
    ];
    let cards = views.map(|(show, size, (col, row))| (show, size, at(&grid, col, row, size, d)));
    unsafe { lay(&cv, &theme, &cards, d) };
    backdrop::set_test(None);
    let (pixels, w, h) = cv.finish();
    kit::save_png(&pixels, w, h, path);
}

#[test]
#[ignore = "offscreen widget spacing PNGs"]
fn widget_motion_spacing_pngs() {
    use crate::theme::Rgba;
    let root = kit::harmony_dir().join("widget-motion");
    std::fs::create_dir_all(&root).unwrap();
    for (wall, dpi, h) in [(Wall::Bright, 96, 1440), (Wall::Day, 144, 1600)] {
        let d = dpi as f32 / 96.;
        let cv = desk(wall, 2560. / d, h as f32 / d, d);
        let mut m = gs::monitor(cv.w, cv.h, d);
        m.work.top = (30. * d).round() as i32;
        m.work.bottom -= (66. * d).round() as i32;
        let pitch = ((76. * d).round() as i32, (96. * d).round() as i32);
        let g = Grid::widget(&m, pitch).unwrap();
        let place = |c, r, size: Size| {
            let (w, h) = size.cells();
            g.rect(Span { c, r, w, h }, 0)
        };
        let theme = kit::theme(dpi == 96);
        unsafe {
            let bar = cv.dc.CreateSolidColorBrush(&card::rgba(Rgba { r: 38, g: 38, b: 38, a: 204 }), None).unwrap();
            cv.dc.FillRectangle(&kit::rect(0., 0., cv.w as f32 / d, 30.), &bar);
            cv.dc.FillRectangle(&kit::rect(0., m.work.bottom as f32 / d, cv.w as f32 / d, h as f32 / d), &bar);
            lay(&cv, &theme, &[
                (Show::Inbox, Size::MEDIUM, place(g.cols - 4, 0, Size::MEDIUM)),
                (Show::Note(gs::MEMO), Size::MEDIUM, place(g.cols - 8, 0, Size::MEDIUM)),
                (Show::Battery, Size::SMALL, place(g.cols - 2, 2, Size::SMALL)),
            ], d);
        }
        backdrop::set_test(None);
        let (pixels, w, h) = cv.finish();
        kit::save_png(&pixels, w, h, &root.join(format!("spacing-{w}x{h}.png")));
    }
}

/// The edit overlay's own painter on a layer of its own, laid over `cv`
/// (as on screen: its window over the cards).
unsafe fn overlay(cv: &kit::Canvas, theme: &crate::theme::Theme, scene: &edit_view::Scene<'_>) {
    let stage = kit::stage();
    let ov = kit::Canvas::of(stage.gfx.d2d_factory().clone(), (cv.w as f32) / scene.monitor.scale(), (cv.h as f32) / scene.monitor.scale(), scene.monitor.scale());
    edit_view::paint(&ov.dc, &stage.gfx, theme, scene).unwrap();
    let (px, w, h) = ov.finish();
    let bmp = gs::bitmap(&cv.dc, &Icon { width: w, height: h, pixels: px });
    let mut base = Matrix3x2::identity();
    cv.dc.GetTransform(&mut base);
    cv.dc.SetTransform(&Matrix3x2::identity());
    cv.dc.DrawImage(&bmp, None, None, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR, D2D1_COMPOSITE_MODE_SOURCE_OVER);
    cv.dc.SetTransform(&base);
}

/// The mock's scene (W x H DIP).
pub const W: f32 = 640.;
pub const H: f32 = 500.;

/// mock5.html's layout: a memo 2 x 1 and a battery 1 x 1 on top, a memo
/// 3 x 1 under them.
pub fn normal_cards(d: f32) -> Vec<(Show, Size, RECT)> {
    let g = grid(d, (4, 2));
    vec![
        (Show::Note(gs::MEMO), Size::MEDIUM, at(&g, 0, 0, Size::MEDIUM, d)),
        (Show::Battery, Size::SMALL, at(&g, 2, 0, Size::SMALL, d)),
        (Show::Note(gs::MEMO), Size::new(3, 1), at(&g, 0, 1, Size::new(3, 1), d)),
    ]
}

/// A scene at rest over `wall`, light or dark.
pub fn normal(wall: Wall, light: bool, d: f32) -> (Vec<u8>, u32, u32) {
    let _lang = crate::i18n::test_lock("ko");
    let theme = kit::theme(light);
    let cv = desk(wall, W, H, d);
    unsafe { lay(&cv, &theme, &normal_cards(d), d) };
    backdrop::set_test(None);
    cv.finish()
}

/// The long press (night, as the mock): a memo 3 x 2 with its white frame,
/// four handles and menu, the free places outlined; `ghost`: a side being
/// dragged, the size it would take outlined.
pub fn resize(wall: Wall, light: bool, d: f32, ghost: Option<Size>) -> (Vec<u8>, u32, u32) {
    let _lang = crate::i18n::test_lock("ko");
    let theme = kit::theme(light);
    let cv = desk(wall, W, H, d);
    let g = grid(d, (4, 2));
    let size = Size::new(3, 2);
    let r = at(&g, 0, 0, size, d);
    unsafe {
        lay(&cv, &theme, &[(Show::Note(MEMO_LONG), size, r)], d);
        let m = gs::monitor(cv.w, cv.h, d);
        let inset = (widget::INSET * d).round() as i32;
        let places = widget::free_slots(&g, inset, &[r]);
        let frames = [(1isize, r)];
        let preview = super::edit::Preview::Note(String::new(), String::new());
        let ghost = ghost.map(|s| at(&g, 0, 0, s, d));
        let scene = edit_view::Scene { monitor: m, frames: &frames, slots: &[], ghost, lift: None, gallery: false, kind: super::edit::Kind::Note, size: Size::SMALL, preview: &preview, preview_hwnd: Default::default(), notes: &[], selected: None, chosen: Some(r), quiet: true, places: &places };
        overlay(&cv, &theme, &scene);
    }
    backdrop::set_test(None);
    cv.finish()
}

/// Every kind in several sizes on one desk (day): note 1x1, 2x1, 2x2, 1x2,
/// 4x1; battery 1x1, 2x1, 1x2, 2x2; inbox 2x1, 2x2, 4x1.
pub fn sizes(wall: Wall, light: bool, d: f32) -> (Vec<u8>, u32, u32) {
    let _lang = crate::i18n::test_lock("ko");
    let theme = kit::theme(light);
    let places = (8, 4);
    let g = grid(d, places);
    let n = Show::Note(gs::MEMO);
    let cards: Vec<(Show, Size, RECT)> = [
        (n, Size::SMALL, 0, 0),
        (n, Size::MEDIUM, 1, 0),
        (Show::Battery, Size::SMALL, 3, 0),
        (Show::Battery, Size::MEDIUM, 4, 0),
        (Show::Inbox, Size::MEDIUM, 6, 0),
        (Show::Note(MEMO_LONG), Size::LARGE, 0, 1),
        (Show::Battery, Size::new(1, 2), 2, 1),
        (Show::Battery, Size::LARGE, 3, 1),
        (Show::Inbox, Size::LARGE, 5, 1),
        (n, Size::new(1, 2), 7, 1),
        (Show::Note(MEMO_LONG), Size::new(4, 1), 0, 3),
        (Show::Inbox, Size::new(4, 1), 4, 3),
    ]
    .into_iter()
    .map(|(s, size, pc, pr)| (s, size, at(&g, pc, pr, size, d)))
    .collect();
    let cv = desk(wall, 36. * 2. + 76. * 16., 40. * 2. + 96. * 8., d);
    unsafe { lay(&cv, &theme, &cards, d) };
    backdrop::set_test(None);
    cv.finish()
}

/// cargo test -p dock desklayer::handling_snapshots -- --ignored
#[test]
#[ignore = "widget handling PNGs: target/ui-snapshots/widget-handling"]
fn widget_handling() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/ui-snapshots/widget-handling");
    std::fs::create_dir_all(&root).unwrap();
    for d in [1., 1.5, 2.] {
        let pc = (d * 100.) as u32;
        let save = |name: &str, (px, w, h): (Vec<u8>, u32, u32)| kit::save_png(&px, w, h, &root.join(format!("{name}-{pc}.png")));
        save("normal-day", normal(Wall::Day, false, d));
        save("normal-night", normal(Wall::Night, false, d));
        save("normal-bright", normal(Wall::Bright, false, d));
        save("normal-light", normal(Wall::Day, true, d));
        save("resize-night", resize(Wall::Night, false, d, None));
        save("resize-day-light", resize(Wall::Day, true, d, None));
        save("resize-dragging-side", resize(Wall::Night, false, d, Some(Size::new(4, 2))));
        save("sizes-day", sizes(Wall::Day, false, d));
        save("sizes-night", sizes(Wall::Night, false, d));
        save("sizes-light", sizes(Wall::Day, true, d));
    }
    println!("widget handling PNGs: {}", root.display());
}

/// What card `show` shows, as its window remembers it (edit::remember).
fn preview(show: Show) -> super::edit::Preview {
    use super::edit::Preview;
    match show {
        Show::Note(text) => Preview::Note(text.into(), "오후 2:10".into()),
        Show::Inbox => {
            let rows = gs::rows();
            let n = rows.len();
            Preview::Inbox(rows, (0..n).collect(), 2)
        }
        Show::Battery => Preview::Battery(Desktop::default(), devices()),
    }
}

/// Card `show` of `size` at `r` (scene px) drawn alone (no shadow):
/// `lying` by its window's own painter, else by the painter of a dragged
/// card (edit_view::face, the drag hint's and edit mode's). Premultiplied
/// BGRA, the card's own px.
fn alone(show: Show, size: Size, r: RECT, d: f32, light: bool, lying: bool) -> Vec<u8> {
    let theme = kit::theme(light);
    let (cw, ch) = (r.right - r.left, r.bottom - r.top);
    let (w, h) = (cw as f32 / d, ch as f32 / d);
    let under = backdrop::under(r);
    super::surface::render(cw, ch, d, |dc, gfx| unsafe {
        if !lying {
            return edit_view::face(dc, gfx, &theme, &preview(show), size, (w, h), under.as_ref());
        }
        match show {
            Show::Note(text) => {
                let f = note::Frame { text, date: "오후 2:10", editing: false, typing: None, alpha: 0., mark: None, under: under.as_ref(), lift: false, shadow: false };
                note::frame(dc, gfx, &theme, &f, w, h)
            }
            Show::Inbox => {
                let rows = gs::rows();
                let visible: Vec<usize> = (0..rows.len()).collect();
                crate::device_battery::view::face(dc, gfx, &theme, card::Kind::Inbox, (w, h), under.as_ref(), 0., |dc, gfx, theme| {
                    widgets::inbox_content(dc, gfx, theme, &widgets::Inbox { rows: &rows, remaining: 2, visible: &visible, hover: None, flash: None }, (w, h), 0.)
                })
            }
            Show::Battery => {
                let devices = devices();
                let list: Vec<&Device> = devices.iter().collect();
                crate::device_battery::view::face(dc, gfx, &theme, card::Kind::Battery, (w, h), under.as_ref(), 0., |dc, gfx, theme| {
                    crate::device_battery::paint::content_sized(dc, gfx, theme, &Desktop { size, ..Default::default() }, &list, (w, h))
                })
            }
        }
    })
    .expect("a picture")
}

/// The largest difference of one channel between two pictures.
fn worst(a: &[u8], b: &[u8]) -> i32 {
    a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap_or(0)
}

/// The cards compared: every kind, before and after a drop (two places)
/// and before and after a resize (each size it takes).
fn compared(d: f32) -> Vec<(String, Show, Size, RECT)> {
    let g = grid(d, (8, 4));
    let mut v = Vec::new();
    for (name, show, sizes) in [
        ("note", Show::Note(gs::MEMO), vec![Size::SMALL, Size::MEDIUM, Size::LARGE, Size::new(3, 2), Size::new(4, 1)]),
        ("inbox", Show::Inbox, vec![Size::MEDIUM, Size::LARGE, Size::new(4, 2)]),
        ("battery", Show::Battery, vec![Size::SMALL, Size::MEDIUM, Size::new(1, 2), Size::LARGE]),
    ] {
        for size in sizes {
            // Where it was picked up, and where it was dropped.
            for (pc, pr) in [(0, 0), (4, 2)] {
                v.push((format!("{name}-{}x{}-at{pc}{pr}", size.w, size.h), show, size, at(&g, pc, pr, size, d)));
            }
        }
    }
    v
}

/// Before and after a drop, before and after a resize: the card drawn by
/// its own window and the card drawn while dragged (the drag hint, edit
/// mode) are the same picture over the card (the lift's 1.03 and deep
/// shadow aside), at 100, 150 and 200 %, dark and light, over the day and
/// the bright wallpapers.
#[test]
fn a_card_looks_the_same_lying_and_dragged() {
    let _lang = crate::i18n::test_lock("ko");
    for d in [1., 1.5, 2.] {
        for wall in [Wall::Day, Wall::Bright] {
            let cv = desk(wall, 36. * 2. + 76. * 16., 40. * 2. + 96. * 8., d);
            drop(cv);
            for light in [false, true] {
                for (name, show, size, r) in compared(d) {
                    let (a, b) = (alone(show, size, r, d, light, true), alone(show, size, r, d, light, false));
                    let w = worst(&a, &b);
                    assert!(w <= 1, "{name} {wall:?} light {light} at {d}: differs by {w}");
                }
            }
            backdrop::set_test(None);
        }
    }
}

/// Review P4: a card dragged across two displays side by side is whole:
/// each display's hint draws its part (at that display's scale, the card
/// at its own), and the parts side by side are the card drawn on one
/// surface. The same and mixed DPI.
#[test]
fn a_card_across_two_displays_is_whole() {
    use crate::monitors::Monitor;
    let _lang = crate::i18n::test_lock("ko");
    let theme = kit::theme(false);
    let cv = desk(Wall::Day, 1200., 600., 1.);
    drop(cv);
    let preview = preview(Show::Note(gs::MEMO));
    let half = |left: i32, dpi: f32, handle: isize| {
        let r = RECT { left, top: 0, right: left + 600, bottom: 600 };
        Monitor { handle, rect: r, work: r, primary: handle == 1, dpi: (96. * dpi) as u32 }
    };
    for (da, db) in [(1., 1.), (1., 2.), (2., 1.), (1.5, 1.)] {
        let (a, b) = (half(0, da, 1), half(600, db, 2));
        // Mostly on A: drawn at A's scale.
        let (w, h) = ((300. * da) as i32, (150. * da) as i32);
        let left = 600 - w * 3 / 5;
        let r = RECT { left, top: 200, right: left + w, bottom: 200 + h };
        let l = edit_view::Lifted { preview: &preview, at: r, size: Size::MEDIUM, dpi: da, t: 1. };
        let part = |m: Monitor| {
            super::surface::render(600, 600, m.scale(), |dc, gfx| unsafe { edit_view::dragged(dc, gfx, &theme, m, &l, backdrop::under_on(r, m).as_ref()) }).expect("a part")
        };
        let (pa, pb) = (part(a), part(b));
        let one = Monitor { handle: 3, rect: RECT { left: 0, top: 0, right: 1200, bottom: 600 }, work: RECT { left: 0, top: 0, right: 1200, bottom: 600 }, primary: true, dpi: 96 };
        let whole = super::surface::render(1200, 600, 1., |dc, gfx| unsafe { edit_view::dragged(dc, gfx, &theme, one, &l, backdrop::under_on(r, one).as_ref()) }).expect("the whole");
        let mut stitched = vec![0u8; whole.len()];
        for y in 0..600usize {
            stitched[y * 4800..y * 4800 + 2400].copy_from_slice(&pa[y * 2400..y * 2400 + 2400]);
            stitched[y * 4800 + 2400..y * 4800 + 4800].copy_from_slice(&pb[y * 2400..y * 2400 + 2400]);
        }
        assert!(pb.chunks(4).any(|p| p[3] > 200), "B shows its part ({da}, {db})");
        let w = worst(&stitched, &whole);
        assert!(w <= 2, "parts differ from the whole by {w} ({da}, {db})");
    }
    backdrop::set_test(None);
}

/// The comparison's pictures (lying | dragged, side by side) for a look.
fn comparison_pngs(root: &std::path::Path) {
    let _lang = crate::i18n::test_lock("ko");
    let dir = root.join("compare");
    std::fs::create_dir_all(&dir).unwrap();
    for d in [1., 2.] {
        let cv = desk(Wall::Day, 36. * 2. + 76. * 16., 40. * 2. + 96. * 8., d);
        drop(cv);
        for (name, show, size, r) in compared(d).into_iter().filter(|c| c.0.ends_with("at00")) {
            let (a, b) = (alone(show, size, r, d, false, true), alone(show, size, r, d, false, false));
            let (w, h) = ((r.right - r.left) as u32, (r.bottom - r.top) as u32);
            let gap = 8u32;
            let mut px = vec![0u8; ((w * 2 + gap) * h * 4) as usize];
            for y in 0..h {
                for (k, src) in [&a, &b].into_iter().enumerate() {
                    let from = (y * w * 4) as usize;
                    let to = ((y * (w * 2 + gap) + k as u32 * (w + gap)) * 4) as usize;
                    px[to..to + (w * 4) as usize].copy_from_slice(&src[from..from + (w * 4) as usize]);
                }
            }
            kit::save_png(&px, w * 2 + gap, h, &dir.join(format!("{name}-{}.png", (d * 100.) as u32)));
        }
        backdrop::set_test(None);
    }
}

/// While dragged (the drag hint's picture: the free places' dots, the
/// landing place, the card lifted 1.03 with its deep shadow), and just
/// after the drop (lying at its place, the glide's end): the mock's
/// bottom left, over `wall`.
pub fn dragging(wall: Wall, light: bool, d: f32, t: f32, settled: bool) -> (Vec<u8>, u32, u32) {
    let _lang = crate::i18n::test_lock("ko");
    let theme = kit::theme(light);
    let cv = desk(wall, W, H, d);
    let g = grid(d, (4, 2));
    let battery = (Show::Battery, Size::SMALL, at(&g, 0, 1, Size::SMALL, d));
    let target = at(&g, 2, 0, Size::MEDIUM, d);
    // Picked up at the top left, carried up right.
    let home = at(&g, 0, 0, Size::MEDIUM, d);
    let (dx, dy) = ((2. * 152. - 26.) * d, -18. * d);
    let held = RECT { left: home.left + dx as i32, top: home.top + dy as i32, right: home.right + dx as i32, bottom: home.bottom + dy as i32 };
    unsafe {
        lay(&cv, &theme, &[battery], d);
        if settled {
            lay(&cv, &theme, &[(Show::Note(gs::MEMO), Size::MEDIUM, target)], d);
        } else {
            let m = gs::monitor(cv.w, cv.h, d);
            let inset = (widget::INSET * d).round() as i32;
            let slots = widget::free_slots(&g, inset, &[battery.2]);
            let now = super::edit::between(held, target, 1. - t);
            if t >= 1. {
                edit_view::marks(&cv.dc, m, Some(target), &slots).unwrap();
            }
            let stage = kit::stage();
            let p = preview(Show::Note(gs::MEMO));
            let l = edit_view::Lifted { preview: &p, at: now, size: Size::MEDIUM, dpi: d, t };
            edit_view::dragged(&cv.dc, &stage.gfx, &theme, m, &l, backdrop::under(now).as_ref()).unwrap();
        }
    }
    backdrop::set_test(None);
    cv.finish()
}

/// cargo test -p dock desklayer::handling_snapshots::widget_handling_drag -- --ignored
#[test]
#[ignore = "widget handling drag PNGs: target/ui-snapshots/widget-handling"]
fn widget_handling_drag() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/ui-snapshots/widget-handling");
    std::fs::create_dir_all(&root).unwrap();
    for d in [1., 1.5, 2.] {
        let pc = (d * 100.) as u32;
        let save = |name: &str, (px, w, h): (Vec<u8>, u32, u32)| kit::save_png(&px, w, h, &root.join(format!("{name}-{pc}.png")));
        save("drag-day", dragging(Wall::Day, false, d, 1., false));
        save("drag-night", dragging(Wall::Night, false, d, 1., false));
        save("drag-light", dragging(Wall::Day, true, d, 1., false));
        save("drag-bright", dragging(Wall::Bright, false, d, 1., false));
        let progress = 1. - super::drag_visual::STILL_LANDING.left(0.06);
        save("drop-settling-60ms", dragging(Wall::Day, false, d, 1. - progress, false));
        save("drop-landed-day", dragging(Wall::Day, false, d, 0., true));
        save("drop-landed-light", dragging(Wall::Day, true, d, 0., true));
    }
    comparison_pngs(&root);
    println!("widget handling drag PNGs: {}", root.display());
}
