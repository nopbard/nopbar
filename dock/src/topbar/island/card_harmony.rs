//! The Live Island's card drawn off screen for the design audit (tests
//! only; see crate::gfx::promo_kit, "harmony").
//!
//! Two kinds of picture. The cards: the card made as `open` makes it (its
//! window never shown) with made-up activities, painted by its own `paint`
//! over a plate drawn the way morph.rs stacks it (the panel's tint, rim,
//! contact ring and E3). The hover and pressed fill is a visual under the
//! picture on screen (Card::sync_wash): here it is drawn where the paint
//! says its target is, in the same token.
//!
//! The morph: the card's own visual tree (morph::Plate, the pill's parts,
//! the card's picture) with `p` set to 0, 0.35, 0.7 and 1, read back from
//! the compositor alone (popup's chrome_harmony::read_back: nothing of the
//! screen, no window shown), over a strip standing for the bar. Each
//! plate is measured against morph::plate_at.
//!
//! Not in the pictures: the progress rows' app icons (the dock's brushes
//! on visuals above the surface; the paint draws a glyph without one).

use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_SEMI_BOLD;
use windows::Win32::UI::WindowsAndMessaging::{DestroyWindow, LoadCursorW, RegisterClassExW, IDC_ARROW, WNDCLASSEXW};

use super::*;
use crate::gfx::promo_kit as kit;
use crate::syslook::{HighContrast, SysLook};
use crate::theme::Rgba;

/// The theme of `look` ("flat", "glass", "hc") in the palette (as the
/// dock's and gfx::parts' sheets resolve it).
pub(super) fn look_theme(look: &str, light: bool) -> Theme {
    let grey = if light { 0x3A } else { 0xE6 };
    let (white, black) = (Rgba { r: 255, g: 255, b: 255, a: 255 }, Rgba { r: 0, g: 0, b: 0, a: 255 });
    let hc = (look == "hc").then_some(HighContrast { window: black, text: white, highlight: Rgba { r: 0x1A, g: 0xEB, b: 0xFF, a: 255 }, highlight_text: black, gray_text: crate::syslook::GRAY_TEXT });
    let sys = SysLook { light, accent: Rgba { r: grey, g: grey, b: grey, a: 255 }, transparency: true, high_contrast: hc, animations: true, ui_font: 0 };
    let mut raw = Theme::graphite();
    // The run's finish for the sheet's own look (kit::look_finish).
    raw.finish = crate::gfx::promo_kit::look_finish(look);
    raw.resolve_fx(&sys, true)
}

pub(super) unsafe fn register() {
    let hinst = GetModuleHandleW(None).unwrap();
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: hinst.into(),
        lpszClassName: w!("NopBarIsland"),
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        ..Default::default()
    };
    RegisterClassExW(&wc);
}

/// A pill on the main display for a card that is never shown: `w` x `h`
/// DIP at `scale`, 2 DIP under the screen's top edge.
pub(super) fn pill_at(stage: &kit::Stage, w: f32, h: f32, scale: f32, parts: Vec<Part>) -> Pill {
    let (l, t) = (500, (2.0 * scale).round() as i32);
    let shell = stage.gfx.compositor.CreateContainerVisual().unwrap().cast().unwrap();
    Pill { rect: RECT { left: l, top: t, right: l + (w * scale).round() as i32, bottom: t + (h * scale).round() as i32 }, scale: 1.0, parts, shell }
}

/// The card for `live` in `theme`, made as `open` makes it, as the audit
/// draws it: the song's own volume known (62 %), left to right.
pub(super) unsafe fn card_for(stage: &kit::Stage, theme: &Theme, live: Vec<Activity>, scale: f32, pill: Pill) -> Card {
    let data = Data { live, icons: HashMap::new(), art: None, pip: false };
    let mut card = Card::new(Request { gfx: stage.gfx.clone(), theme: theme.clone(), scale, pill, bar: stage.hwnd, data, pinned: true }).expect("the card");
    card.rtl = false;
    card.volume = Some((String::new(), 62, false));
    card.vol_answered = true;
    card.focus = None;
    card.relayout();
    card
}

/// One card, `set` up, painted at `x`, `y` (DIP) of `cv`, a canvas at the
/// card's scale, over its plate.
pub(super) unsafe fn paint_card(cv: &kit::Canvas, x: f32, y: f32, card: &Card, theme: &Theme) {
    use windows_numerics::Matrix3x2;
    let s = card.scale;
    let mut plate = kit::popup_plate(theme, theme.radii().panel, crate::theme::Elevation::E3);
    // (The rim at this canvas's scale, not the film's: Theme::rim_width.)
    let rim = plate.rim.take();
    kit::draw_plate(cv, x, y, W, card.height, &plate);
    if let Some(stops) = rim {
        kit::draw_rim_px(cv, x, y, W, card.height, plate.radius, stops, theme.rim_width(crate::theme::Material::Panel, s));
    }
    // Where the wash is on screen: under the picture, on its target's
    // shape (a first paint tells where; nothing of it is kept).
    let mut dry = Painted::default();
    let scratch = kit::Canvas::at(W, card.height, s);
    card.paint(&scratch.dc, &mut dry).expect("painted");
    let _ = scratch.finish();
    let on = card.pressed.map(|t| (t, State::Pressed)).or(card.hover.map(|t| (t, State::Hover))).filter(|(t, _)| !t.slider() && *t != Target::Shot(0) && card.hc().is_none());
    if let Some((_, r, rad)) = on.and_then(|(t, _)| dry.shapes.iter().find(|s| s.0 == t)) {
        let k = if card.pressed.is_some() { crate::motion::press_scale(r.right - r.left, r.bottom - r.top) } else { 1.0 };
        let (cx, cy, hw, hh) = ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0, (r.right - r.left) / 2.0 * k, (r.bottom - r.top) / 2.0 * k);
        cv.fill_rr(kit::rect(x + cx - hw, y + cy - hh, x + cx + hw, y + cy + hh), rad * k, theme.fill_of(Kind::Row, on.unwrap().1));
    }
    let mut out = Painted::default();
    cv.dc.SetTransform(&Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: x * s, M32: y * s });
    let painted = card.paint(&cv.dc, &mut out);
    cv.dc.SetTransform(&Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: 0.0, M32: 0.0 });
    painted.expect("painted");
}

fn shoot(name: &str, live: impl Fn() -> Vec<Activity>, set: impl Fn(&mut Card)) {
    let stage = kit::stage();
    unsafe {
        register();
        for (folder, light) in kit::LOOKS {
            let theme = look_theme("flat", light);
            let mut card = card_for(&stage, &theme, live(), 1.0, pill_at(&stage, 120.0, 28.0, 1.0, Vec::new()));
            card.scale = kit::scale();
            set(&mut card);
            let cv = kit::harmony_canvas(&stage, W, card.height, light);
            paint_card(&cv, kit::HARMONY_M, kit::HARMONY_M, &card, &theme);
            kit::harmony_save(cv, folder, name);
            let _ = DestroyWindow(card.hwnd);
            assert!(kit::harmony_dir().join(folder).join(format!("{name}.png")).is_file());
        }
    }
}

fn focus_timer() -> Vec<Activity> {
    let v = crate::focus_timer::View { phase: 0, left_ms: (18 * 60 + 42) * 1000, total_ms: 25 * 60_000, running: true, deadline: 1, today: 2, no_dnd: false };
    vec![Activity::Timer(model::Timer::of_focus(v))]
}

#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_island_cards() {
    let _l = crate::i18n::test_lock("ko");
    // A song playing and a copy (the promo film's), play / pause hovered.
    shoot("island-card-media", super::promo::live, |c| c.hover = Some(Target::Toggle));
    // The focus timer, 18:42 of 25 minutes left, pause hovered.
    shoot("island-card-timer", focus_timer, |c| c.hover = Some(Target::Timer(1)));
    shoot("island-card-battery", || vec![Activity::Battery(Power::Charging(64))], |_| {});
    shoot("island-card-battery-low", || vec![Activity::Battery(Power::Low(12))], |_| {});
    // The camera and the microphone in one app, the microphone in another.
    let uses = || {
        let u = |sensor, id: &str, app: &str| model::Use { sensor, id: id.into(), app: app.into() };
        vec![Activity::Privacy(vec![u(model::Sensor::Camera, "meet", "화상 회의"), u(model::Sensor::Microphone, "meet", "화상 회의"), u(model::Sensor::Microphone, "rec", "녹음기")])]
    };
    shoot("island-card-privacy", uses, |_| {});
    // Three jobs, the second one's row hovered.
    let jobs = || {
        let j = |key: &str, name: &str, files: bool, ratio: f32| model::Job { key: key.into(), name: name.into(), files, ratio, hwnd: 1 };
        vec![Activity::Progress(vec![j("a", "파일", true, 0.64), j("b", "브라우저", false, 0.31), j("c", "설치 프로그램", false, 0.92)])]
    };
    shoot("island-card-multi", jobs, |c| c.hover = Some(Target::Job(0, 1)));
    // The focus timer over: its name over today's count.
    let over = || {
        let v = crate::focus_timer::View { phase: 2, left_ms: 0, total_ms: 25 * 60_000, running: false, deadline: 1, today: 3, no_dnd: false };
        vec![Activity::Timer(model::Timer::of_focus(v))]
    };
    shoot("island-card-timer-over", over, |_| {});
}

/// The states inventory (final/states-inventory.md, 2026-11-11): the
/// song's volume at 0 and 100 % and muted, the keyboard's ring on the
/// volume and on the position bar, long names cut, a song without a
/// timeline, right to left.
#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_island_card_values() {
    let _l = crate::i18n::test_lock("ko");
    let song = || vec![super::promo::live().remove(0)];
    shoot("island-card-volume-0", song, |c| c.volume = Some((String::new(), 0, false)));
    shoot("island-card-volume-100", song, |c| c.volume = Some((String::new(), 100, false)));
    shoot("island-card-volume-muted", song, |c| c.volume = Some((String::new(), 40, true)));
    shoot("island-card-volume-focus", song, |c| {
        c.keyboard = true;
        c.focus = Some(Target::Volume);
    });
    shoot("island-card-seek-focus", song, |c| {
        c.keyboard = true;
        c.focus = Some(Target::Seek);
    });
    let long = || {
        let mut v = vec![super::promo::live().remove(0)];
        if let Some(Activity::Media(t)) = v.first_mut() {
            t.title = "아주 길고 긴 노래 제목이 카드 끝까지 가고도 남는 경우의 제목".into();
            t.artist = "하모니 필하모닉 오케스트라와 합창단 그리고 친구들".into();
        }
        v
    };
    shoot("island-card-media-long", long, |_| {});
    let no_time = || {
        let mut v = vec![super::promo::live().remove(0)];
        if let Some(Activity::Media(t)) = v.first_mut() {
            t.timeline = None;
        }
        v
    };
    shoot("island-card-media-no-timeline", no_time, |_| {});
    let jobs = || {
        let j = |key: &str, name: &str, files: bool, ratio: f32| model::Job { key: key.into(), name: name.into(), files, ratio, hwnd: 1 };
        vec![Activity::Progress(vec![j("a", "아주 긴 이름의 압축 파일 풀기 작업 2026년 11월 백업.zip", true, 0.0), j("b", "브라우저 다운로드", false, 1.0)])]
    };
    shoot("island-card-multi-long", jobs, |_| {});
    drop(_l);
    let _l = crate::i18n::test_lock("ar");
    shoot("island-card-media-rtl", song, |c| {
        c.rtl = true;
        c.relayout();
        c.keyboard = true;
        c.focus = Some(Target::Volume);
    });
}

/// The interaction table (6.2) on the card, one card for each state, in
/// the three looks at 100 and 150 %: rest; play / pause under the
/// pointer; pressed (the fill darker, its size press_scale); the
/// keyboard on it (the ring); the volume held (its knob KNOB_HELD larger)
/// with the position bar under the pointer (its knob shows); a song with
/// nothing before it (previous cannot be used) and no volume yet.
#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_island_card_states() {
    let _l = crate::i18n::test_lock("ko");
    let stage = kit::stage();
    type Set = fn(&mut Card);
    let states: [Set; 6] = [
        |_| {},
        |c| c.hover = Some(Target::Toggle),
        |c| {
            c.hover = Some(Target::Toggle);
            c.pressed = Some(Target::Toggle);
        },
        |c| {
            c.keyboard = true;
            c.focus = Some(Target::Toggle);
        },
        |c| {
            c.sliding = Some(Target::Volume);
            c.hover = Some(Target::Seek);
        },
        |c| {
            if let Some(Activity::Media(t)) = c.data.live.first_mut() {
                t.can_prev = false;
            }
            c.volume = None;
            c.vol_answered = false;
        },
    ];
    unsafe {
        register();
        for (look, scale) in [("flat", 1.0f32), ("flat", 1.5), ("glass", 1.5), ("hc", 1.5)] {
            for (folder, light) in kit::LOOKS {
                let theme = look_theme(look, light);
            // DWM's blur stands in as the picture's backdrop (glass::StandIn).
            let _ground = crate::glass::StandIn::new(kit::harmony_backdrop(light));
                let m = kit::HARMONY_M;
                let mut cards = Vec::new();
                for set in states {
                    let mut card = card_for(&stage, &theme, vec![super::promo::live().remove(0)], 1.0, pill_at(&stage, 120.0, 28.0, 1.0, Vec::new()));
                    card.scale = scale;
                    set(&mut card);
                    cards.push(card);
                }
                let h = cards.iter().map(|c| c.height).fold(0.0, f32::max);
                let (cols, rows) = (3usize, 2usize);
                let cv = kit::Canvas::at(cols as f32 * (W + m) + m, rows as f32 * (h + m) + m, scale);
                cv.fill_rr(kit::rect(0.0, 0.0, cols as f32 * (W + m) + m, rows as f32 * (h + m) + m), 0.0, kit::harmony_backdrop(light));
                for (i, card) in cards.iter().enumerate() {
                    paint_card(&cv, m + (i % cols) as f32 * (W + m), m + (i / cols) as f32 * (h + m), card, &theme);
                }
                let pct = (scale * 100.0).round() as u32;
                let name = if look == "flat" { format!("island-card-states-{pct}") } else { format!("island-card-states-{look}-{pct}") };
                let (px, w, hh) = cv.finish();
                kit::save_png(&px, w, hh, &kit::harmony_dir().join(folder).join(format!("{name}.png")));
                for card in cards {
                    let _ = DestroyWindow(card.hwnd);
                }
            }
        }
    }
}

/// The pill of the promo film's song (island::promo draws the same one in
/// D2D): its width, and its parts as the island hands them to the card.
fn song_pill(stage: &kit::Stage, theme: &Theme, scale: f32, h: f32) -> (f32, Vec<Part>) {
    use super::super::{eq_color, pill_colors, EQ_BARS, EQ_GAP, EQ_H, EQ_W, G_MUSIC as LEAD_GLYPH, LEAD, TEXT_SIZE};
    let gfx = &stage.gfx;
    let (_, ink, ..) = pill_colors(theme);
    let live = super::promo::live();
    let (words, _) = model::pill_words(&live[0]);
    let col = rgba(ink);
    let brush = |s: &windows::UI::Composition::CompositionDrawingSurface| -> CompositionBrush {
        let b = gfx.compositor.CreateSurfaceBrushWithSurface(s).unwrap();
        b.SetStretch(CompositionStretch::Fill).unwrap();
        b.cast().unwrap()
    };
    let (glyph, gw, gh) = gfx.glyph(LEAD_GLYPH, LEAD, col, scale).unwrap();
    let (text, tw, th, base, _) = gfx.text_line(&words, &theme.font.family, TEXT_SIZE, DWRITE_FONT_WEIGHT_SEMI_BOLD, col, scale, model::TITLE_MAX, theme.high_contrast, None).unwrap();
    let eq_w = EQ_BARS as f32 * EQ_W + (EQ_BARS - 1) as f32 * EQ_GAP;
    let c = model::Content { lead: gw.max(gh), text: tw, text_h: th, trail: eq_w, trail_h: EQ_H, dots: 0, baseline: Some(base) };
    let w = model::snap(model::pill_width(&c), scale);
    let f = model::frame(false, w, h, &c, scale);
    let mut parts = vec![
        Part::Picture { brush: brush(&glyph), x: f.lead.x, y: f.lead.y, w: f.lead.w, h: f.lead.h, radius: 0.0 },
        Part::Picture { brush: brush(&text), x: f.text.x, y: f.text.y, w: tw, h: th, radius: 0.0 },
    ];
    for i in 0..EQ_BARS {
        parts.push(Part::Fill { color: eq_color(theme), x: f.trail.x + i as f32 * (EQ_W + EQ_GAP), y: f.trail.y + EQ_H * 0.45, w: EQ_W, h: EQ_H * 0.55 });
    }
    (w, parts)
}

/// Lets this thread's queue run for `ms` (the compositor commits from it).
unsafe fn pump(ms: u64) {
    let end = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < end {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            DispatchMessageW(&msg);
        }
        std::thread::sleep(Duration::from_millis(4));
    }
}

/// The morph's still frames (7.2 rule 4): the real plate at p = 0, 0.35,
/// 0.7, 1 side by side, each over a strip standing for the bar, read back
/// from the compositor. `look`: "flat", "glass" or "hc".
fn morph_sheet(name: &str, look: &str, scale: f32) {
    use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
    let stage = kit::stage();
    let c = stage.gfx.compositor.clone();
    // The still frames, then two that ride the real spring while the
    // sheet is read back (half a second: long past GLIDE): one sent from
    // the pill to the card, and one sent to the card and, 80 ms on, back
    // (the plate turned round in place; it must end as the pill).
    const PS: [f32; 6] = [0.0, 0.35, 0.7, 1.0, 1.0, 0.0];
    const SPRUNG: usize = 4;
    let (bar_h, m) = (32.0f32, 56.0f32);
    unsafe {
        register();
        let interop: ICompositorDesktopInterop = c.cast().unwrap();
        let target = interop.CreateDesktopWindowTarget(stage.hwnd, false).unwrap();
        let root = c.CreateContainerVisual().unwrap();
        target.SetRoot(&root).unwrap();
        for (folder, light) in kit::LOOKS {
            let theme = look_theme(look, light);
            // DWM's blur stands in as the picture's backdrop (glass::StandIn).
            let _ground = crate::glass::StandIn::new(kit::harmony_backdrop(light));
            // At rest on whole device pixels, as island_layout has it.
            let pill_h = model::rest_height(bar_h, scale);
            let mut cards = Vec::new();
            for (i, p) in PS.into_iter().enumerate() {
                let (pw, parts) = song_pill(&stage, &theme, scale, pill_h);
                let mut card = card_for(&stage, &theme, super::promo::live(), scale, pill_at(&stage, pw, pill_h, scale, parts));
                assert_eq!(card.fit, 1.0, "the main display has room for the card");
                card.redraw();
                if i < SPRUNG {
                    card.plate.jump(p);
                }
                cards.push((p, card));
            }
            let h = cards[0].1.height;
            let cell = W + 2.0 * m;
            let (w_px, h_px) = ((cell * PS.len() as f32 * scale).ceil() as i32, ((h + bar_h + 2.0 * m) * scale).ceil() as i32);
            root.Children().unwrap().RemoveAll().unwrap();
            let scene = c.CreateContainerVisual().unwrap();
            scene.SetSize(Vector2 { X: w_px as f32, Y: h_px as f32 }).unwrap();
            root.Children().unwrap().InsertAtTop(&scene).unwrap();
            let sprite = |col: Rgba, x: f32, y: f32, w: f32, hh: f32| {
                let v = c.CreateSpriteVisual().unwrap();
                v.SetBrush(&c.CreateColorBrushWithColor(windows::UI::Color { A: 255, R: col.r, G: col.g, B: col.b }).unwrap()).unwrap();
                v.SetOffset(Vector3 { X: x, Y: y, Z: 0.0 }).unwrap();
                v.SetSize(Vector2 { X: w, Y: hh }).unwrap();
                scene.Children().unwrap().InsertAtTop(&v).unwrap();
            };
            sprite(kit::harmony_backdrop(light), 0.0, 0.0, w_px as f32, h_px as f32);
            // The bar: its tint over the backdrop, as wide as the sheet.
            let bar_top = (m * scale).round();
            let bar = theme.material(crate::theme::Material::Bar);
            let under = kit::harmony_backdrop(light);
            let k = bar.a as f32 / 255.0;
            let mix = |a: u8, b: u8| (a as f32 * k + b as f32 * (1.0 - k)).round() as u8;
            sprite(Rgba { r: mix(bar.r, under.r), g: mix(bar.g, under.g), b: mix(bar.b, under.b), a: 255 }, 0.0, bar_top, w_px as f32, (bar_h * scale).round());
            let mut spots = Vec::new();
            for (i, (p, card)) in cards.iter().enumerate() {
                // The card's own tree, out of its (never shown) window:
                // scaled as its window scales it, its top on the pill's.
                let holder = c.CreateContainerVisual().unwrap();
                holder.SetScale(Vector3 { X: scale, Y: scale, Z: 1.0 }).unwrap();
                let (ox, oy) = (((i as f32 * cell + m) * scale).round(), bar_top + (model::PILL_INSET * scale).round());
                holder.SetOffset(Vector3 { X: ox, Y: oy, Z: 0.0 }).unwrap();
                card.outer.Children().unwrap().Remove(&card.anim).unwrap();
                holder.Children().unwrap().InsertAtTop(&card.anim).unwrap();
                scene.Children().unwrap().InsertAtTop(&holder).unwrap();
                let plate = morph::plate_at(pill_shape(card.pill, 1.0, card.rect, card.ds()), card.shape(), *p);
                spots.push((*p, ox, oy, plate));
            }
            if crate::motion::full() {
                for (_, card) in cards.iter_mut().skip(SPRUNG) {
                    assert!(card.plate.go(1.0, crate::motion::SPRING_GLIDE) > 100, "the spring takes its time");
                }
                pump(80);
                let (p, card) = cards.last_mut().unwrap();
                let (at, _) = card.plate.progress();
                assert!(at > 0.1 && at < 0.95, "under way when it is turned round: {at}");
                card.plate.go(*p, crate::motion::SPRING_GLIDE);
            } else {
                for (p, card) in cards.iter_mut().skip(SPRUNG) {
                    card.plate.jump(*p);
                }
            }
            let px = crate::popup::chrome_harmony::read_back(&scene, w_px, h_px).unwrap_or_else(|| panic!("{name}: the visual could not be read back: not drawn"));
            kit::save_png(&px, w_px as u32, h_px as u32, &kit::harmony_dir().join(folder).join(format!("{name}.png")));
            // Each plate is where plate_at says: just inside its left and
            // right edges is the plate, just outside is not (under it the
            // shadow may be as dark as the plate: not measured).
            let lum = |x: f32, y: f32| px[((y.round() as i32).clamp(0, h_px - 1) * w_px + (x.round() as i32).clamp(0, w_px - 1)) as usize * 4 + 1] as i32;
            // (High contrast: the pill is Window on a Window bar; its outline
            // is all there is to see. Not measured.)
            for (p, ox, oy, s) in spots.into_iter().filter(|_| look != "hc") {
                let (l, r, mid) = (ox + s.x * scale, ox + (s.x + s.w) * scale, oy + (s.y + s.h / 2.0) * scale);
                // (Dark Flat v2: mid-way the plate's M4 and M2, with the
                // card's grain coming, can sit within 3 of the shadowed
                // backdrop; then its edge is drawn by its rims, 1 DIP just
                // inside it: that line stands out from both sides, where
                // plate_at says.)
                // (A glass's pill at rest has no rim, and on Clear dark its
                // coat can be the bar's own lightness: then its edge is drawn
                // by the contact ring just outside it, darker than both
                // sides, where plate_at says. final/results.md 7.)
                let d = 3.0 * scale;
                let parts = |edge: f32, ring: f32, inside: f32, outside: f32| {
                    let (i, o) = (lum(inside, mid), lum(outside, mid));
                    let lit = (-1..=1).map(|k| lum(edge + k as f32 * 0.5 * scale, mid)).max().unwrap_or(0);
                    let dark = (-2..=2).map(|k| lum(ring + k as f32 * 0.5 * scale, mid)).min().unwrap_or(255);
                    (i - o).abs() >= 3 || (lit - i.max(o) >= 3) || (i.min(o) - dark >= 3)
                };
                // (The rim lies just inside the edge: half a DIP in; the
                // contact ring just outside it: a DIP out.)
                assert!(parts(l + 0.5 * scale, l - scale, l + d, l - d), "{name} {folder} p {p}: the plate's left edge at {l}: {} / {}", lum(l - d, mid), lum(l + d, mid));
                assert!(parts(r - 0.5 * scale, r + scale, r - d, r + d), "{name} {folder} p {p}: the plate's right edge at {r}: {} / {}", lum(r - d, mid), lum(r + d, mid));
            }
            for (_, card) in cards {
                let _ = DestroyWindow(card.hwnd);
            }
        }
        drop(target);
    }
}

#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_island_morph() {
    let _l = crate::i18n::test_lock("ko");
    morph_sheet("island-morph-100", "flat", 1.0);
    morph_sheet("island-morph-150", "flat", 1.5);
    morph_sheet("island-morph-glass-150", "glass", 1.5);
    morph_sheet("island-morph-hc-150", "hc", 1.5);
    // A run at its own scale (HARMONY_SCALE=1.25, 1.75: the 32 bar's pill
    // there is 32.5 and 45.5 px, kept on whole ones) in its own finish.
    if std::env::var_os("HARMONY_SCALE").is_some() {
        morph_sheet(&format!("island-morph-run-{}", (kit::scale() * 100.0).round()), "flat", kit::scale());
    }
}

/// Last-two review P2 (2026-11-11): the pill at rest, stacked as
/// TopBar::island_layout stacks it (its fill in the capsule, its rim's
/// shape visual: edge_for, Theme::rim_width wide) on a bar of each height
/// from 20 to 48, at kit::scale() in the run's finish (HARMONY_FINISH),
/// read back from the compositor alone. Each row: the pill as it was
/// (pill_height and place()'s width as they come, its rim in its own
/// light), then as it rests now (model::rest_at) in its own light and in
/// solid ink. The solid one is measured: a one-pixel rim is one row
/// (column) of full ink on each straight side, top and bottom alike, left
/// and right alike.
#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_island_pill_rims() {
    use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
    let _l = crate::i18n::test_lock("ko");
    let stage = kit::stage();
    let c = stage.gfx.compositor.clone();
    let s = kit::scale();
    const BARS: [f32; 6] = [20.0, 24.0, 26.0, 28.0, 32.0, 48.0];
    // A width that is no whole number of pixels at any scale.
    const WANT: f32 = 151.37;
    let (m, col_w) = (24.0f32, WANT + 48.0);
    let (w_px, h_px) = (((2.0 * m + 3.0 * col_w) * s).ceil() as i32, ((2.0 * m + BARS.iter().map(|b| b + 12.0).sum::<f32>()) * s).ceil() as i32);
    unsafe {
        register();
        let interop: ICompositorDesktopInterop = c.cast().unwrap();
        let target = interop.CreateDesktopWindowTarget(stage.hwnd, false).unwrap();
        let root = c.CreateContainerVisual().unwrap();
        target.SetRoot(&root).unwrap();
        for (folder, light) in kit::LOOKS {
            let theme = look_theme("flat", light);
            let rim_w = theme.rim_width(crate::theme::Material::Tip, s);
            let ink = super::super::pill_colors(&theme).1;
            let fill = super::super::pill_fill(&theme, super::super::Fill::Rest);
            root.Children().unwrap().RemoveAll().unwrap();
            let scene = c.CreateContainerVisual().unwrap();
            scene.SetSize(Vector2 { X: w_px as f32, Y: h_px as f32 }).unwrap();
            root.Children().unwrap().InsertAtTop(&scene).unwrap();
            let solid = |col: Rgba| -> CompositionBrush { c.CreateColorBrushWithColor(windows::UI::Color { A: col.a, R: col.r, G: col.g, B: col.b }).unwrap().cast().unwrap() };
            let sprite = |col: Rgba, x: f32, y: f32, w: f32, hh: f32| {
                let v = c.CreateSpriteVisual().unwrap();
                v.SetBrush(&solid(col)).unwrap();
                v.SetOffset(Vector3 { X: x, Y: y, Z: 0.0 }).unwrap();
                v.SetSize(Vector2 { X: w, Y: hh }).unwrap();
                scene.Children().unwrap().InsertAtTop(&v).unwrap();
            };
            let under = kit::harmony_backdrop(light);
            sprite(under, 0.0, 0.0, w_px as f32, h_px as f32);
            let bar = theme.material(crate::theme::Material::Bar);
            let k = bar.a as f32 / 255.0;
            let mix = |a: u8, b: u8| (a as f32 * k + b as f32 * (1.0 - k)).round() as u8;
            let bar_col = Rgba { r: mix(bar.r, under.r), g: mix(bar.g, under.g), b: mix(bar.b, under.b), a: 255 };
            // The rim's light, as island_draw makes it.
            let light_brush = |stops: [Rgba; 3]| -> CompositionBrush {
                let g = c.CreateLinearGradientBrush().unwrap();
                g.SetStartPoint(Vector2 { X: 0.0, Y: 0.0 }).unwrap();
                g.SetEndPoint(Vector2 { X: 0.0, Y: 1.0 }).unwrap();
                for (at, col) in crate::theme::RIM_STOPS.iter().zip(stops) {
                    g.ColorStops().unwrap().Append(&c.CreateColorGradientStopWithOffsetAndColor(*at, super::super::color(col)).unwrap()).unwrap();
                }
                g.cast().unwrap()
            };
            let mut measured = Vec::new();
            let mut row_px = (m * s).round();
            for bar_h in BARS {
                sprite(bar_col, 0.0, row_px, w_px as f32, (bar_h * s).round());
                for col in 0..3 {
                    // The bar's left on a whole pixel; place()'s x a
                    // fraction in.
                    let left_px = ((m + col as f32 * col_w) * s).round();
                    let x = 12.3;
                    let (cx, top, w, h) = if col > 0 {
                        model::rest_at(x, WANT, bar_h, s)
                    } else {
                        let h = model::pill_height(bar_h);
                        (model::snap(x + WANT / 2.0, s), crate::topbar::baseline::cell_top(bar_h, h, s), WANT, h)
                    };
                    let f = model::frame(false, w, h, &model::Content::default(), s);
                    // The pill's root, in a holder scaled as the bar is.
                    let holder = c.CreateContainerVisual().unwrap();
                    holder.SetOffset(Vector3 { X: left_px, Y: row_px, Z: 0.0 }).unwrap();
                    holder.SetScale(Vector3 { X: s, Y: s, Z: 1.0 }).unwrap();
                    scene.Children().unwrap().InsertAtTop(&holder).unwrap();
                    let pill = c.CreateContainerVisual().unwrap();
                    pill.SetOffset(Vector3 { X: cx, Y: top, Z: 0.0 }).unwrap();
                    holder.Children().unwrap().InsertAtTop(&pill).unwrap();
                    let shape = |geom: model::Rect, r: f32, fill_b: Option<&CompositionBrush>, stroke: Option<(&CompositionBrush, f32)>, at: model::Rect| {
                        let v = c.CreateShapeVisual().unwrap();
                        v.SetOffset(Vector3 { X: at.x, Y: at.y, Z: 0.0 }).unwrap();
                        v.SetSize(Vector2 { X: at.w, Y: at.h }).unwrap();
                        let g = c.CreateRoundedRectangleGeometry().unwrap();
                        g.SetOffset(Vector2 { X: geom.x, Y: geom.y }).unwrap();
                        g.SetSize(Vector2 { X: geom.w, Y: geom.h }).unwrap();
                        g.SetCornerRadius(Vector2 { X: r, Y: r }).unwrap();
                        let sh = c.CreateSpriteShapeWithGeometry(&g).unwrap();
                        if let Some(b) = fill_b {
                            sh.SetFillBrush(b).unwrap();
                        }
                        if let Some((b, t)) = stroke {
                            sh.SetStrokeBrush(b).unwrap();
                            sh.SetStrokeThickness(t).unwrap();
                        }
                        v.Shapes().unwrap().Append(&sh).unwrap();
                        pill.Children().unwrap().InsertAtTop(&v).unwrap();
                    };
                    // The fill: the capsule (island_layout clips a fill to it).
                    shape(model::Rect { x: 0.0, y: 0.0, w: f.pill.w, h: f.pill.h }, h / 2.0, Some(&solid(fill)), None, f.pill);
                    // The rim: half its width in (edge_for), in the edge's box.
                    let e = super::super::edge_for(&f, rim_w);
                    let rim_b = match (col, super::super::pill_rim(&theme)) {
                        (2, _) | (_, None) => solid(ink),
                        (_, Some(stops)) => light_brush(stops),
                    };
                    shape(e, e.h / 2.0, None, Some((&rim_b, rim_w)), f.edge_box);
                    if col == 2 {
                        // Its straight sides (px in the scene).
                        measured.push((bar_h, left_px + (cx + f.pill.x) * s, row_px + top * s, w * s, h * s));
                    }
                }
                row_px += ((bar_h + 12.0) * s).round();
            }
            let px = crate::popup::chrome_harmony::read_back(&scene, w_px, h_px).unwrap_or_else(|| panic!("the pills could not be read back: not drawn"));
            let name = format!("island-pill-rims-{}", (s * 100.0).round());
            kit::save_png(&px, w_px as u32, h_px as u32, &kit::harmony_dir().join(folder).join(format!("{name}.png")));
            // A one-pixel rim (a glass's): the row (column) just inside each
            // straight side all ink, top as bottom, left as right.
            if (rim_w * s - 1.0).abs() < 1e-3 {
                let lum = |x: i32, y: i32| px[((y.clamp(0, h_px - 1) * w_px + x.clamp(0, w_px - 1)) * 4 + 1) as usize] as i32;
                let want = ink.g as i32;
                for (bar_h, l, t, w, h) in measured {
                    let (mx, my) = ((l + w / 2.0).floor() as i32, (t + h / 2.0).floor() as i32);
                    let (l, t, r, b) = (l.round() as i32, t.round() as i32, (l + w).round() as i32 - 1, (t + h).round() as i32 - 1);
                    let sides = [("top", lum(mx, t)), ("bottom", lum(mx, b)), ("left", lum(l, my)), ("right", lum(r, my))];
                    eprintln!("{folder} {s} bar {bar_h}: ink {want}, {sides:?}");
                    for (side, v) in sides {
                        assert!((v - want).abs() <= 6, "{folder} {s} bar {bar_h}: the {side} rim {v}, ink {want}");
                    }
                }
            }
        }
        drop(target);
    }
}

/// The pill pressed again while the card opens (7.2: the morph turns round
/// where it is), through the card's own window procedure on a real card:
/// the press is read on the plate as drawn (not the card's final place),
/// the island is told, and its Dismiss (close_into) sends the morph from
/// where it is back to 0. Not a picture: nothing is written.
#[test]
#[ignore = "builds the card on the compositor: cargo test -p dock island_reclick -- --ignored"]
fn island_reclick_turns_the_morph_round() {
    let _l = crate::i18n::test_lock("ko");
    let stage = kit::stage();
    unsafe {
        register();
        // What the card told the island (posted to the stage's window).
        let heard = || {
            let mut out = Vec::new();
            let mut m = MSG::default();
            while PeekMessageW(&mut m, Some(stage.hwnd), WM_ISLAND_ACT, WM_ISLAND_ACT, PM_REMOVE).as_bool() {
                out.push(m.wParam.0);
            }
            out
        };
        let _ = heard();
        let theme = look_theme("flat", false);
        let card = card_for(&stage, &theme, super::promo::live(), 1.0, pill_at(&stage, 120.0, 28.0, 1.0, Vec::new()));
        let hwnd = card.hwnd;
        CARD.with(|c| *c.borrow_mut() = Some(card));
        // A left click at card DIP x, y (the window's client px).
        let click = |x: f32, y: f32| {
            let (px, py) = with_card(|c| ((c.m_px as f32 + x * c.ds()).round() as i32, (c.m_px as f32 + y * c.ds()).round() as i32)).unwrap();
            let lp = LPARAM((((py as u32 & 0xFFFF) << 16) | (px as u32 & 0xFFFF)) as isize);
            wndproc(hwnd, WM_LBUTTONDOWN, WPARAM(1), lp);
            wndproc(hwnd, WM_LBUTTONUP, WPARAM(0), lp);
        };
        // Out by hovering, just leaving the pill.
        with_card(|c| {
            c.pinned = false;
            c.redraw();
            c.go(1.0);
        });
        assert!(with_card(|c| c.opening()).unwrap(), "on its way out");
        let (vx, vy) = with_card(|c| {
            let r = c.hits.iter().find(|h| h.1 == Target::Volume).expect("the volume").0;
            ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0)
        })
        .unwrap();
        // Where the volume will be, not drawn yet: off the plate (it closes
        // as any press outside does), and the volume is not touched.
        click(vx, vy);
        assert_eq!(with_card(|c| (c.sliding, c.volume.clone())).unwrap(), (None, Some((String::new(), 62, false))));
        assert_eq!(heard(), [ACT_DISMISS]);
        // The pill pressed again: a card out by hovering stays (ACT_PIN).
        let pill = with_card(|c| c.plate.drawn().pill).unwrap();
        let (px, py) = (pill.x + pill.w / 2.0, pill.y + pill.h / 2.0);
        click(px, py);
        assert_eq!(heard(), [ACT_PIN]);
        // Out by a click: pressed again, it goes back (ACT_DISMISS).
        with_card(|c| c.pinned = true);
        assert!(with_card(|c| c.opening()).unwrap());
        click(px, py);
        assert_eq!(heard(), [ACT_DISMISS]);
        // The island's Dismiss: back into the pill from where it is.
        let (at, to) = with_card(|c| c.plate.progress()).unwrap();
        assert_eq!(to, 1.0);
        close_into(pill_at(&stage, 120.0, 28.0, 1.0, Vec::new()));
        let (back, to) = with_card(|c| c.plate.progress()).unwrap();
        assert_eq!(to, 0.0, "the morph's target is the pill");
        assert!(back >= at - 0.05, "turned round where it was ({at} -> {back}), not started over");
        assert!(with_card(|c| c.closing).unwrap() && !is_open());
        if let Some(mut c) = CARD.with(|c| c.borrow_mut().take()) {
            c.cancel_timers();
            let _ = DestroyWindow(c.hwnd);
        }
    }
}

/// The ring shows only after a key that moves the focus (crate::input):
/// media, volume and letter keys leave it off, Tab and the arrows show
/// it, a press of the pointer (either button, through the window's own
/// handler) hides it; a screen reader's focus moves the focus and shows
/// the ring only as it was, or with Windows' keyboard cues always on.
#[test]
fn the_ring_shows_only_after_a_key_that_moves_the_focus() {
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_DOWN, VK_TAB};
    let _l = crate::i18n::test_lock("ko");
    let stage = kit::stage();
    unsafe {
        register();
        let theme = look_theme("flat", false);
        let card = card_for(&stage, &theme, super::promo::live(), 1.0, pill_at(&stage, 120.0, 28.0, 1.0, Vec::new()));
        let hwnd = card.hwnd;
        CARD.with(|c| *c.borrow_mut() = Some(card));
        // A press on the card's own plate, off every target: its top
        // left corner, a few DIP in.
        let lp = with_card(|c| {
            let (px, py) = ((c.m_px as f32 + 6.0 * c.ds()).round() as i32, (c.m_px as f32 + 6.0 * c.ds()).round() as i32);
            LPARAM((((py as u32 & 0xFFFF) << 16) | (px as u32 & 0xFFFF)) as isize)
        })
        .unwrap();
        struct S(HWND, LPARAM, bool);
        let mut s = S(hwnd, lp, false);
        let ring = |_: &S| with_card(|c| c.keyboard).unwrap();
        let key = |_: &mut S, vk: u16| run(with_card(|c| c.on_key(vk)).flatten());
        let press = |s: &mut S| {
            let (down, up) = if s.2 { (WM_RBUTTONDOWN, WM_RBUTTONUP) } else { (WM_LBUTTONDOWN, WM_LBUTTONUP) };
            s.2 = !s.2;
            wndproc(s.0, down, WPARAM(1), s.1);
            wndproc(s.0, up, WPARAM(0), s.1);
        };
        crate::input::sample::sequence("island card", &mut s, VK_TAB.0, VK_DOWN.0, ring, key, press);
        let src = include_str!("card.rs");
        assert!(src.contains("let act = with_card(|c| c.on_key(vk))"), "the window's keys go through on_key");
        assert!(src.contains("c.keyboard = crate::input::reader_focus(c.keyboard);"));
        let card = CARD.with(|c| c.borrow_mut().take());
        if let Some(c) = card {
            let _ = DestroyWindow(c.hwnd);
        }
    }
}
