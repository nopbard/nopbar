//! Menus read back off screen for the design audit (tests only; see
//! crate::gfx::promo_kit, "harmony"): levels made as `track` makes them
//! (Tracker::build_level: their windows are never shown, nothing is
//! tracked), put in the state a picture asks for (the pointer on a row, a
//! press, the keyboard's cursor, a submenu up, scrolled), and then their
//! own Composition trees are read (GraphicsCaptureItem::CreateFromVisual:
//! nothing of the screen). So a picture is what the product builds: the
//! plate (popup::chrome_at), the rows, the fills with their own colors and
//! shapes, a slider's well and knob, a failure's button, the bands a long
//! menu fades under. Nothing is drawn again here.
//!
//! The rows are laid out against the primary display's work area, as on
//! screen: a menu taller than it would go compact there too.

use windows::Win32::Foundation::SYSTEMTIME;
use windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTOPRIMARY;

use super::*;
use crate::gfx::promo_kit as kit;
use crate::syslook::{HighContrast, SysLook};
use crate::theme::Rgba;

/// What a picture shows besides the rows.
#[derive(Clone, Default)]
pub(crate) struct Shot {
    /// The row (by its label) under the pointer.
    pub hover: Option<String>,
    /// The row (by its label) whose submenu is open.
    pub sub: Option<String>,
    /// The submenu's row under the pointer.
    pub sub_hover: Option<String>,
    /// What the clock row says.
    pub clock: Option<SYSTEMTIME>,
    /// What the pointer is on, when it is not a whole row (a day, a month
    /// header's button, a failure's button).
    pub point: Option<Target>,
    /// The button is down on what the pointer is on.
    pub press: bool,
    /// The row (by its label) the keyboard's cursor is on.
    pub key: Option<String>,
    /// Hangs in a room this tall (DIP; 0: the whole work area), scrolled
    /// this far.
    pub room: f32,
    pub scroll: f32,
}

impl Shot {
    pub(crate) fn hover(label: &str) -> Shot {
        Shot { hover: Some(label.into()), ..Default::default() }
    }
}

/// The label of the `n`-th row that can be picked (0: the first).
pub(crate) fn nth_row(items: &[Item], n: usize) -> Option<String> {
    items.iter().filter(|it| it.selectable()).nth(n).map(|it| it.label.clone())
}

/// The theme of `look` ("flat", "glass", "hc") in the palette (as
/// gfx::parts' token sheets and the bar's pictures resolve it).
pub(crate) fn look_theme(look: &str, light: bool) -> Theme {
    let grey = if light { 0x3A } else { 0xE6 };
    let (white, black) = (Rgba { r: 255, g: 255, b: 255, a: 255 }, Rgba { r: 0, g: 0, b: 0, a: 255 });
    let hc = (look == "hc").then_some(HighContrast { window: black, text: white, highlight: Rgba { r: 0x1A, g: 0xEB, b: 0xFF, a: 255 }, highlight_text: black, gray_text: crate::syslook::GRAY_TEXT });
    let sys = SysLook { light, accent: Rgba { r: grey, g: grey, b: grey, a: 255 }, transparency: true, high_contrast: hc, animations: true, ui_font: 0 };
    let mut raw = Theme::graphite();
    // The run's finish for the sheet's own look (kit::look_finish).
    raw.finish = crate::gfx::promo_kit::look_finish(look);
    raw.resolve_fx(&sys, true).windows_shell()
}

/// A tracker that tracks nothing: levels are built with it.
fn tracker(stage: &kit::Stage, theme: &Theme, scale: f32) -> Tracker {
    Tracker {
        gfx: stage.gfx.clone(),
        theme: theme.clone(),
        scale,
        levels: Vec::new(),
        titles: Titles::default(),
        done: None,
        align: Align::TopLeft,
        down_row: None,
        down_on: None,
        down_in: false,
        on_slide: None,
        on_space: None,
        sliding: None,
        drag_ids: 0..0,
        down_at: None,
        // As Tracker::new: the language's direction (the right to left
        // pictures were drawn left to right: states-issues).
        rtl: crate::i18n::rtl(),
        sub_left: crate::i18n::rtl(),
        sub_dir: crate::i18n::rtl(),
        companion_body: None,
        edge: EdgeScroll::default(),
        anchor: POINT::default(),
        hangs: false,
        on_pick: None,
        // (Nothing grows open: the picture is of the menu at rest.)
        instant: true,
        pending_sub: None,
        swap_in: true,
        sub_row: None,
        turn: 0,
    }
}

/// One menu of a picture, built and in its state: its levels (never
/// shown), and their box (DIP from the first level's top left).
struct Scene {
    t: Tracker,
    /// (x0, y0, x1, y1).
    bounds: (f32, f32, f32, f32),
}

impl Drop for Scene {
    fn drop(&mut self) {
        for l in self.t.levels.drain(..) {
            unsafe {
                let _ = DestroyWindow(l.hwnd);
            }
        }
    }
}

unsafe fn scene(stage: &kit::Stage, theme: &Theme, scale: f32, items: &[Item], shot: &Shot) -> Scene {
    register_class();
    let mut t = tracker(stage, theme, scale);
    // The primary display's work area, from its top left corner; in a
    // room, hanging that far over its bottom.
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    let _ = GetMonitorInfoW(MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY), &mut mi);
    let mut at = POINT { x: mi.rcWork.left, y: mi.rcWork.top };
    if shot.room > 0.0 {
        t.hangs = true;
        at.y = mi.rcWork.bottom - (shot.room * scale).round() as i32;
    }
    t.anchor = at;
    CLOCK_AT.with(|c| c.set(shot.clock));
    let first = t.build_level(items.to_vec(), at, None, None).expect("the menu");
    t.levels.push(first);
    if t.levels[0].clock.is_some() {
        t.draw_clock(0);
    }
    CLOCK_AT.with(|c| c.set(None));
    if shot.scroll > 0.0 {
        t.scroll_level(0, shot.scroll);
    }
    let row_of = |l: &Level, name: &str| l.items.iter().position(|it| it.label == name);
    // Its submenu, where Tracker::open_sub opens it; its row stays open.
    if let Some(row) = shot.sub.as_deref().and_then(|n| row_of(&t.levels[0], n)) {
        let l = &t.levels[0];
        let parent = l.rect;
        let (ry, rh) = l.rows[row];
        let row_y = ry + l.view.shift();
        let rows = l.items[row].sub.clone();
        let at = sub_anchor(parent, row_y, t.rtl, scale);
        t.sub_row = Some(RECT { left: parent.left, right: parent.right, top: parent.top + (row_y * scale).round() as i32, bottom: parent.top + ((row_y + rh) * scale).round() as i32 });
        let sub = t.build_level(rows, at, Some(parent), None).expect("the submenu");
        t.levels.push(sub);
        t.levels[0].open_row = Some(row);
        t.levels[0].hover = Some(row);
        if let Some(r) = shot.sub_hover.as_deref().and_then(|n| row_of(&t.levels[1], n)) {
            t.levels[1].hover = Some(r);
            t.levels[1].pointer = t.levels[1].items[r].selectable().then_some(Target::Row(r));
        }
        t.sync_glow(1);
    }
    // The pointer, a press, the keyboard's cursor.
    let point = shot.point.or_else(|| {
        let r = shot.hover.as_deref().and_then(|n| row_of(&t.levels[0], n))?;
        t.levels[0].items[r].selectable().then_some(Target::Row(r))
    });
    if let Some(p) = point {
        t.levels[0].pointer = Some(p);
        t.levels[0].hover = Some(p.row());
        if shot.press {
            t.levels[0].pressed = Some(p);
        }
    }
    if let Some(r) = shot.key.as_deref().and_then(|n| row_of(&t.levels[0], n)) {
        t.levels[0].by_key = true;
        t.levels[0].hover = Some(r);
    }
    t.sync_glow(0);
    let s = scale;
    let first = t.levels[0].rect;
    let mut b = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    for l in &t.levels {
        let r = l.rect;
        let (x, y) = ((r.left - first.left) as f32 / s, (r.top - first.top) as f32 / s);
        b = (b.0.min(x), b.1.min(y), b.2.max(x + (r.right - r.left) as f32 / s), b.3.max(y + (r.bottom - r.top) as f32 / s));
    }
    Scene { t, bounds: b }
}

/// Menus side by side, read back to
/// `target/harmony-shots/<light|dark>/<name>.png` for `look` at `scale`.
/// Returns the first menu's size (DIP). Where the compositor cannot be
/// read back the run fails: nothing drawn is never a pass.
pub(crate) fn compose(name: &str, look: &str, scale: f32, menus: &[(&[Item], Shot)]) -> (f32, f32) {
    let mut size = (0.0, 0.0);
    for (folder, light) in kit::LOOKS {
        let theme = if look == "film" { kit::theme(light).windows_shell() } else { look_theme(look, light) };
        let (read, w_px, h_px, first) = read_menus(&theme, light, scale, menus);
        size = first;
        // Not read back: a failure, never a run that passes without its
        // picture (final review 4).
        let px = read.unwrap_or_else(|| panic!("{name}: the compositor could not be read back: not drawn"));
        let path = kit::harmony_dir().join(folder).join(format!("{name}.png"));
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        kit::save_png(&px, w_px as u32, h_px as u32, &path);
        assert!(path.is_file());
    }
    size
}

/// `menus` side by side in `theme` over the picture's backdrop, read back:
/// (BGRA pixels or None where the compositor cannot be read back, width
/// and height in px, the first menu's size in DIP).
pub(crate) fn read_menus(theme: &Theme, light: bool, scale: f32, menus: &[(&[Item], Shot)]) -> (Option<Vec<u8>>, i32, i32, (f32, f32)) {
    use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
    let stage = kit::stage();
    let c = &stage.gfx.compositor;
    let (m, gap) = (kit::HARMONY_M, space::S6);
    // DWM's blur stands in as the picture's backdrop (glass::StandIn).
    let _ground = crate::glass::StandIn::new(kit::harmony_backdrop(light));
    unsafe {
        let scenes: Vec<Scene> = menus.iter().map(|(items, shot)| scene(&stage, theme, scale, items, shot)).collect();
        let w: f32 = scenes.iter().map(|s| s.bounds.2 - s.bounds.0).sum::<f32>() + gap * (scenes.len() as f32 - 1.0) + m * 2.0;
        let h: f32 = scenes.iter().map(|s| s.bounds.3 - s.bounds.1).fold(0.0, f32::max) + m * 2.0;
        let (w_px, h_px) = ((w * scale).ceil() as i32, (h * scale).ceil() as i32);
        let interop: ICompositorDesktopInterop = c.cast().unwrap();
        // The stage's window is never shown: only what the visuals hang on.
        let target = interop.CreateDesktopWindowTarget(stage.hwnd, false).unwrap();
        let root = c.CreateContainerVisual().unwrap();
        root.SetSize(Vector2 { X: w_px as f32, Y: h_px as f32 }).unwrap();
        target.SetRoot(&root).unwrap();
        let back = c.CreateSpriteVisual().unwrap();
        let bg = kit::harmony_backdrop(light);
        back.SetBrush(&c.CreateColorBrushWithColor(Color { A: 255, R: bg.r, G: bg.g, B: bg.b }).unwrap()).unwrap();
        back.SetSize(Vector2 { X: w_px as f32, Y: h_px as f32 }).unwrap();
        root.Children().unwrap().InsertAtTop(&back).unwrap();
        let mut x = m;
        for sc in &scenes {
            let first = sc.t.levels[0].rect;
            for l in &sc.t.levels {
                // Out of its own (hidden) window's tree, into the picture:
                // its plate's top left where it is on screen, relative to
                // the first level's.
                let _ = l._target.SetRoot(None::<&windows::UI::Composition::Visual>);
                let m_px = (SHADOW_M * scale).ceil();
                let holder = c.CreateContainerVisual().unwrap();
                let (dx, dy) = ((l.rect.left - first.left) as f32, (l.rect.top - first.top) as f32);
                holder.SetOffset(Vector3 { X: ((x - sc.bounds.0) * scale).round() + dx - m_px, Y: ((m - sc.bounds.1) * scale).round() + dy - m_px, Z: 0.0 }).unwrap();
                holder.Children().unwrap().InsertAtTop(&l._root).unwrap();
                root.Children().unwrap().InsertAtTop(&holder).unwrap();
            }
            x += sc.bounds.2 - sc.bounds.0 + gap;
        }
        let read = chrome_harmony::read_back(&root, w_px, h_px);
        let size = (scenes[0].bounds.2 - scenes[0].bounds.0, scenes[0].bounds.3 - scenes[0].bounds.1);
        drop(scenes);
        drop(target);
        (read, w_px, h_px, size)
    }
}

/// A see-through menu read back as the product builds it: its plate is
/// one color from its middle to its bottom (its shadows under it never
/// show through: on screen the backdrop brush covers them, here its stand
/// in), on every finish, light and dark (final/issues.md 1).
#[test]
#[ignore = "reads the compositor back: cargo test -p dock harmony_ -- --ignored"]
fn harmony_a_menus_plate_is_even() {
    use crate::theme::Finish;
    let _l = crate::i18n::test_lock("ko");
    let items: Vec<Item> = (1..=12).map(|i| Item::new(i, format!("{i}"))).collect();
    let s = 1.5;
    for finish in [Finish::Flat, Finish::Glass] {
        for (_, light) in kit::LOOKS {
            let mut raw = Theme::graphite();
            raw.finish = finish;
            let theme = raw.resolve_fx(&SysLook { light, transparency: true, ..Default::default() }, true);
            let (read, w, h, (mw, mh)) = read_menus(&theme, light, s, &[(&items, Shot::default())]);
            // A check run on purpose (--ignored) that cannot read what it
            // checks fails: it never passes having looked at nothing, nor
            // stops before the other finishes (final review 4).
            let px = read.unwrap_or_else(|| panic!("{finish:?} light {light}: the compositor could not be read back: nothing was checked"));
            assert_eq!(px.len(), (w * h * 4) as usize, "{finish:?} light {light}: the whole picture");
            // Beside the rows' text (their left gutter), down the plate.
            let at = |y: f32| px[((((kit::HARMONY_M + y) * s) as i32 * w + ((kit::HARMONY_M + 6.0) * s) as i32) * 4) as usize] as i32;
            // What was read is the scene: the picture's backdrop in its
            // corner, outside the menu and its shadows.
            let bg = kit::harmony_backdrop(light);
            assert!((px[0] as i32 - bg.b as i32).abs() <= 2 && px[3] == 255, "{finish:?} light {light}: the backdrop {:?}, read {:?}", bg, &px[..4]);
            let (mid, low) = (at(mh * 0.5), at(mh - 6.0));
            let fall = theme.flat_fill(crate::theme::Material::Menu);
            let spread = fall.map_or(0, |(a, b)| (a.r as i32 - b.r as i32).abs());
            // (A glass's tone falls to the bottom: glass::SPEC_LINE.)
            // (Its white over the plate: spec * (255 - plate) times how much
            // further down the bottom row lies.)
            let tone = theme.glass_fx(crate::theme::Material::Menu).map_or(0, |f| (f.spec * (255 - mid) as f32 * (0.5 - 6.0 / mh)).ceil() as i32);
            assert!((mid - low).abs() <= spread + tone + 3, "{finish:?} light {light}: the middle {mid}, the bottom {low} ({mw} x {mh})");
            // And it is its tint over what the backdrop shows (the picture's
            // grey through the glass's graph), nothing darker under it; an
            // opaque plate is its own fall (Flat v2's top to bottom).
            let tint = theme.material(crate::theme::Material::Menu);
            match kit::plate_ground(&theme, crate::theme::Material::Menu, tint.a == 255) {
                Some(g) => {
                    let want = crate::theme::over(tint, g).r as i32;
                    assert!((low - want).abs() <= 3, "{finish:?} light {light}: the bottom {low}, its tint over its backdrop {want}");
                }
                None => {
                    let (a, b) = fall.map_or((tint.r as i32, tint.r as i32), |(a, b)| (a.r as i32, b.r as i32));
                    assert!((a.min(b) - 3..=a.max(b) + 3).contains(&low), "{finish:?} light {light}: the bottom {low}, the plate {a}..{b}");
                }
            }
        }
    }
}

/// `items` as a menu, light and dark, to
/// `target/harmony-shots/<look>/<name>.png`. Returns the first level's
/// size (DIP).
pub(crate) fn shoot(name: &str, items: &[Item], shot: &Shot) -> (f32, f32) {
    compose(name, "film", kit::scale(), &[(items, shot.clone())])
}

/// A menu with every kind of plain row: a header, a check, a glyph, a
/// shortcut, a disabled row, a submenu, a line that only says something.
fn rows() -> Vec<Item> {
    vec![
        Item::new(0, "보기").head(),
        Item::new(1, "큰 아이콘").checked(true),
        Item::new(2, "작은 아이콘"),
        Item::new(3, "새로 고침").shortcut("F5"),
        Item::sep(),
        Item::new(0, "정렬").head(),
        Item::new(4, "이름순"),
        Item::new(5, "날짜순").enabled(false),
        Item::new(0, "더 보기").submenu(vec![Item::new(6, "종류순"), Item::new(7, "크기순").checked(true), Item::sep(), Item::new(8, "내림차순")]),
        Item::sep(),
        Item::new(9, "설정 열기…").shortcut("Ctrl+,"),
    ]
}

#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_menu_rows() {
    let _l = crate::i18n::test_lock("ko");
    let items = rows();
    shoot("menu-rows", &items, &Shot::hover("작은 아이콘"));
    shoot("menu-rows-submenu", &items, &Shot { sub: Some("더 보기".into()), sub_hover: Some("종류순".into()), ..Default::default() });
}

/// The states of a row (design system 6.2), one menu each: at rest; the
/// pointer on a row (the row fill); the button down on it (the pressed
/// fill, at once, no size change); the keyboard's cursor (the selected
/// fill, no ring); a submenu up (its row keeps the pressed fill, the
/// pointer's hover is in the submenu, 4 from its parent); the pointer on
/// a row that cannot be used (nothing). At 100 and 150 %, and on Glass and
/// in high contrast at 150 %.
#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_menu_states() {
    let _l = crate::i18n::test_lock("ko");
    let items = rows();
    let on = |f: fn(&mut Shot)| {
        let mut s = Shot::hover("작은 아이콘");
        f(&mut s);
        s
    };
    let menus: Vec<(&[Item], Shot)> = vec![
        (&items, Shot::default()),
        (&items, on(|_| {})),
        (&items, on(|s| s.press = true)),
        (&items, Shot { key: Some("새로 고침".into()), ..Default::default() }),
        (&items, Shot { sub: Some("더 보기".into()), sub_hover: Some("종류순".into()), ..Default::default() }),
        (&items, Shot::hover("날짜순")),
    ];
    for (name, look, scale) in [("menu-states-100", "flat", 1.0), ("menu-states-150", "flat", 1.5), ("menu-states-glass-150", "glass", 1.5), ("menu-states-hc-150", "hc", 1.5)] {
        compose(name, look, scale, &menus);
    }
}

/// The states inventory (final/states-inventory.md, 2026-11-11): a row
/// too long for the menu (cut, its shortcut kept), the keyboard's cursor
/// on a disabled-looking neighbour, right to left (mirrored: checks and
/// arrows at the other end).
#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_menu_values() {
    let _l = crate::i18n::test_lock("ko");
    let mut items = rows();
    items.insert(3, Item::new(20, "아주 길고 긴 메뉴 항목 이름이 메뉴의 폭을 넘어서는 경우의 이름").shortcut("Ctrl+Shift+L"));
    shoot("menu-rows-long", &items, &Shot { key: Some("아주 길고 긴 메뉴 항목 이름이 메뉴의 폭을 넘어서는 경우의 이름".into()), ..Default::default() });
    drop(_l);
    let _l = crate::i18n::test_lock("ar");
    let items = rows();
    shoot("menu-rows-rtl", &items, &Shot { sub: Some("더 보기".into()), sub_hover: Some("종류순".into()), key: None, ..Default::default() });
    shoot("menu-rows-rtl-key", &items, &Shot { key: Some("새로 고침".into()), ..Default::default() });
}

/// A menu taller than its room (2.5): at its top (rows fade out at the
/// bottom only), in the middle (at both edges), at its end (at the top
/// only). No arrows, no lines: the band is an alpha mask on the rows. High
/// contrast: no band, a 1 DIP line at a cut edge.
#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_menu_scroll() {
    let _l = crate::i18n::test_lock("ko");
    let mut items: Vec<Item> = (1..=24).map(|i| Item::new(i, format!("창 {i}")).checked(i == 3)).collect();
    items.insert(8, Item::sep());
    let at = |scroll: f32, hover: &str| Shot { room: 220.0, scroll, hover: Some(hover.into()), ..Default::default() };
    let menus: Vec<(&[Item], Shot)> = vec![(&items, at(0.0, "창 2")), (&items, at(150.0, "창 10")), (&items, at(9999.0, "창 23"))];
    for (name, look, scale) in [("menu-scroll-100", "flat", 1.0), ("menu-scroll-150", "flat", 1.5), ("menu-scroll-glass-150", "glass", 1.5), ("menu-scroll-hc-150", "hc", 1.5)] {
        compose(name, look, scale, &menus);
    }
}

/// Quiet states in a menu (design system 11): a list still being fetched
/// (the ring alone), one that failed (a warning glyph, one line, a button:
/// at rest, under the pointer, pressed), an empty one (one tertiary line),
/// and a slider (a well and a raised knob).
#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_menu_quiet() {
    let _l = crate::i18n::test_lock("ko");
    let foot = || vec![Item::sep(), Item::new(9, "네트워크 설정…")];
    let with = |first: Item| std::iter::once(first).chain(foot()).collect::<Vec<Item>>();
    let busy = with(Item::busy("불러오는 중"));
    let failed = with(Item::failed(1, "목록을 불러오지 못했어요", "다시 시도"));
    let empty = with(Item::quiet("최근 항목 없음"));
    let slider = vec![Item::slider(0.4), Item::sep(), Item::new(2, "음소거"), Item::new(3, "소리 설정…")];
    let menus: Vec<(&[Item], Shot)> = vec![
        (&busy, Shot::default()),
        (&failed, Shot::default()),
        (&failed, Shot { point: Some(Target::Button(0)), ..Default::default() }),
        (&failed, Shot { point: Some(Target::Button(0)), press: true, ..Default::default() }),
        (&failed, Shot { key: Some("목록을 불러오지 못했어요".into()), ..Default::default() }),
        (&empty, Shot::default()),
        (&slider, Shot::hover("음소거")),
    ];
    for (name, look, scale) in [("menu-quiet-100", "flat", 1.0), ("menu-quiet-150", "flat", 1.5), ("menu-quiet-hc-150", "hc", 1.5)] {
        compose(name, look, scale, &menus);
    }
}

/// `items` as a menu level as `track` builds it (Flat, dark, 100 %), read
/// by screen readers: its UIA nodes.
pub(crate) fn a11y_nodes(items: &[Item]) -> Vec<crate::a11y::Node> {
    let stage = kit::stage();
    let sc = unsafe { scene(&stage, &look_theme("flat", false), 1.0, items, &Shot::default()) };
    sc.t.a11y_tree(0).nodes
}

/// A failure's row and Windows' page under it.
fn failure() -> Vec<Item> {
    vec![Item::failed(1, "목록을 불러오지 못했어요", "다시 시도"), Item::sep(), Item::new(9, "네트워크 설정…")]
}

/// Screen points (px) in level 0 of `t`: the middle of the failure's
/// button, and a point on its text on the same line.
fn failure_points(t: &Tracker) -> (POINT, POINT) {
    let l = &t.levels[0];
    let s = t.scale;
    let (_, b, (bx, by)) = &l.buttons[0];
    let (bw, bh) = b.size();
    let y = l.rect.top + ((by + bh / 2.0 + l.view.shift()) * s).round() as i32;
    let button = POINT { x: l.rect.left + ((bx + bw / 2.0) * s).round() as i32, y };
    let text = POINT { x: l.rect.left + ((CONTENT_X + 24.0) * s).round() as i32, y };
    (button, text)
}

/// The design review's P2 (2026-11-11): the button drew its text and
/// outline once, at rest. In high contrast, pressed, its text stayed
/// WindowText on the Highlight fill (gone where the two are the same) and
/// under the pointer the 1 DIP WindowText line stayed instead of 2 DIP
/// Highlight (12.3). Its visuals are read as the menu's state moves.
#[test]
fn a_failure_s_button_shows_each_state_s_text_and_outline() {
    let stage = kit::stage();
    for look in ["hc", "flat"] {
        let theme = look_theme(look, false);
        let mut sc = unsafe { scene(&stage, &theme, 1.5, &failure(), &Shot::default()) };
        let t = &mut sc.t;
        let r = t.levels[0].buttons[0].0;
        let mut seen = Vec::new();
        for (pointer, pressed, state) in [(None, None, State::Rest), (Some(Target::Button(r)), None, State::Hover), (Some(Target::Button(r)), Some(Target::Button(r)), State::Pressed), (None, None, State::Rest)] {
            t.levels[0].pointer = pointer;
            t.levels[0].pressed = pressed;
            t.sync_glow(0);
            let shows = t.levels[0].buttons[0].1.shows();
            let want = (Some(theme.text_of(crate::theme::Level::Primary, state)), theme.outline_of(crate::theme::Role::Raised, state));
            assert_eq!(shows, want, "{look}: {state:?}");
            seen.push(shows);
        }
        if look == "hc" {
            let h = theme.hc.unwrap();
            // At rest: WindowText, a 1 DIP WindowText line; under the
            // pointer a 2 DIP Highlight line; pressed: HighlightText on
            // the Highlight fill, no line.
            assert_eq!(seen[0], (Some(h.text), Some((h.text, 1.0))));
            assert_eq!(seen[1], (Some(h.text), Some((h.highlight, 2.0))));
            assert_eq!(seen[2], (Some(h.highlight_text), None));
            assert_eq!(seen[3], seen[0]);
        } else {
            // Outside high contrast: the primary text in every state, no
            // line.
            assert!(seen.iter().all(|s| *s == (Some(theme.text_primary()), None)));
        }
    }
}

/// The review's P2 5 (2026-11-11): the menus' raised button had the opaque
/// card fill but not the card's light. Flat v2: the drop under it and the
/// light along its top at rest, the light gone at once pressed; high
/// contrast: none.
#[test]
fn a_failure_s_button_wears_the_cards_light() {
    let stage = kit::stage();
    for look in ["flat", "hc"] {
        let theme = look_theme(look, false);
        let mut sc = unsafe { scene(&stage, &theme, 1.5, &failure(), &Shot::default()) };
        let t = &mut sc.t;
        let r = t.levels[0].buttons[0].0;
        let rest = t.levels[0].buttons[0].1.card_shows();
        (t.levels[0].pointer, t.levels[0].pressed) = (Some(Target::Button(r)), Some(Target::Button(r)));
        t.sync_glow(0);
        let pressed = t.levels[0].buttons[0].1.card_shows();
        match theme.card_fx(State::Rest) {
            Some(fx) => {
                assert_eq!(rest, (fx.drop, fx.light), "{look}: at rest");
                assert_eq!(pressed, (fx.drop, Rgba { a: 0, ..fx.light }), "{look}: pressed");
            }
            None => assert!(rest.0.a == 0 && rest.1.a == 0 && pressed.0.a == 0 && pressed.1.a == 0, "{look}"),
        }
    }
}

/// The panels' review (2026-11-11, P2; the menus left it): with the
/// keyboard's cursor on a failure row, high contrast lays the Highlight
/// fill under the button, which kept WindowText and its Highlight line
/// there. Now it answers on the row: HighlightText and HighlightText lines,
/// inverted again pressed; back on Window when the cursor goes.
#[test]
fn a_failure_s_button_answers_on_its_row_s_cursor() {
    let stage = kit::stage();
    for look in ["hc", "flat"] {
        let theme = look_theme(look, false);
        let mut sc = unsafe { scene(&stage, &theme, 1.5, &failure(), &Shot::default()) };
        let t = &mut sc.t;
        let r = t.levels[0].buttons[0].0;
        let b = Some(Target::Button(r));
        // (cursor on the row, pointer, pressed, the button's state, the row's)
        let steps = [
            (false, None, None, State::Rest, State::Rest),
            (true, None, None, State::Rest, State::Cursor),
            (true, b, None, State::Rest, State::Cursor),
            (false, b, None, State::Hover, State::Rest),
            (false, b, b, State::Pressed, State::Rest),
            (true, None, None, State::Rest, State::Cursor),
            (false, None, None, State::Rest, State::Rest),
        ];
        let mut seen = Vec::new();
        for (key, pointer, pressed, state, ground) in steps {
            let l = &mut t.levels[0];
            (l.hover, l.by_key, l.pointer, l.pressed) = (Some(r), key, pointer, pressed);
            t.sync_glow(0);
            assert_eq!(parts::ground_of(r, t.levels[0].shown.mark), ground, "{look}: the row under it");
            let shows = t.levels[0].buttons[0].1.shows();
            let want = (Some(theme.text_on(crate::theme::Level::Primary, state, ground)), theme.outline_on(crate::theme::Role::Raised, state, ground));
            assert_eq!(shows, want, "{look}: {state:?} on {ground:?}");
            seen.push(shows);
        }
        if look == "hc" {
            let h = theme.hc.unwrap();
            assert_eq!(seen[0], (Some(h.text), Some((h.text, 1.0))), "on Window");
            assert_eq!(seen[1], (Some(h.highlight_text), Some((h.highlight_text, 1.0))), "on the Highlight");
            assert_eq!(seen[6], seen[0], "the cursor went");
        } else {
            assert!(seen.iter().all(|s| *s == (Some(theme.text_primary()), None)));
        }
    }
    // Pure: only the row's own instant fill is under it.
    assert_eq!(parts::ground_of(3, Some((Target::Row(3), parts::Tone::Cursor))), State::Cursor);
    assert_eq!(parts::ground_of(3, Some((Target::Row(2), parts::Tone::Cursor))), State::Rest);
    assert_eq!(parts::ground_of(3, Some((Target::Button(3), parts::Tone::Pressed))), State::Rest);
    assert_eq!(parts::ground_of(3, None), State::Rest);
}

/// The design review's P2 (2026-11-11): a press that began on a
/// failure's text and was let go on its button asked again (the release
/// checked the row only). Now a press in the menu picks only when it
/// began on the button; one that opened the menu from the bar still lets
/// the release pick.
#[test]
fn a_failure_s_button_picks_only_a_press_that_began_on_it() {
    let stage = kit::stage();
    let theme = look_theme("hc", false);
    let mut sc = unsafe { scene(&stage, &theme, 1.5, &failure(), &Shot::default()) };
    let t = &mut sc.t;
    let picks = std::rc::Rc::new(std::cell::Cell::new(0));
    let n = picks.clone();
    t.on_pick = Some(Box::new(move |id, _| {
        assert_eq!(id, 1);
        n.set(n.get() + 1);
        true
    }));
    let r = t.levels[0].buttons[0].0;
    let (button, text) = failure_points(t);
    assert_eq!((t.target_at(0, Some(r), button), t.target_at(0, Some(r), text)), (Some(Target::Button(r)), None));
    assert_eq!((t.hit(button), t.hit(text)), (Some((0, Some(r))), Some((0, Some(r)))));
    unsafe {
        // Down on the text, up on the button: nothing.
        t.on_button(text, true, true);
        t.on_move(button);
        t.on_button(button, false, true);
        assert_eq!(picks.get(), 0, "a press begun on the text");
        // Down on the button, up on it: once; it showed pressed meanwhile.
        t.on_button(button, true, true);
        assert_eq!(t.levels[0].buttons[0].1.shows().0, Some(theme.hc.unwrap().highlight_text));
        t.on_button(button, false, true);
        assert_eq!(picks.get(), 1);
        // Down on the button, up on the text: nothing.
        t.on_button(button, true, true);
        t.on_move(text);
        t.on_button(text, false, true);
        assert_eq!(picks.get(), 1);
        // Down in the menu's margin (no row), up on the button: nothing.
        let l = &t.levels[0];
        let margin = POINT { x: (l.rect.left + l.rect.right) / 2, y: l.rect.top + (2.0 * t.scale).round() as i32 };
        assert_eq!(t.hit(margin), Some((0, None)), "the margin is in the menu, on no row");
        t.on_button(margin, true, true);
        t.on_move(button);
        t.on_button(button, false, true);
        assert_eq!(picks.get(), 1, "a press begun in the margin");
        // The press opened the menu from the bar (none began here): the
        // release on the button picks, like a menu bar.
        t.on_button(button, false, true);
        assert_eq!(picks.get(), 2);
    }
    assert_eq!(t.done, None);
}
