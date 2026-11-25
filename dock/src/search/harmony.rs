//! The search panel drawn off screen for the design audit (tests only; see
//! crate::gfx::promo_kit, "harmony"): the panel made as `show` makes it
//! (Panel::make: its window never shown), with workers that do nothing
//! (no app list, no index, no icons: every row keeps its glyph), its text
//! and results put in by hand (made up), painted by its own `paint` over
//! the plate popup::chrome gives it (drawn here). The rows' fills and the
//! note's button are visuals on screen (crate::wash): drawn here from the
//! same numbers; the progress ring likewise (gfx::spinner, standing still).
//!
//! The scenes (design system 12.4): results with the first row selected
//! (200, 100 and 150 %); the field alone; the pointer on another row, and
//! pressing it; nothing found; answers late (the ring); the file search
//! gone wrong (its note, the pointer on "Try again"; the keyboard's cursor
//! on the note); a long list scrolled under the field; Glass; high
//! contrast.

use super::*;
use crate::gfx::promo_kit as kit;
use crate::wash::harmony as shots;

/// Workers that never run: what is sent to them goes nowhere.
fn idle(_: &Arc<Shared>) -> Workers {
    Workers { quick: channel().0, files: channel().0, icons: channel().0 }
}

fn hit(kind: Kind, title: &str, detail: &str) -> Hit {
    Hit { kind, title: title.into(), detail: detail.into(), target: format!("harmony:{title}"), icon: None, score: 0 }
}

/// What a scene shows.
struct Scene<'a> {
    text: &'a str,
    quick: &'a [Hit],
    files: &'a [Hit],
    state: FilesState,
    /// The work area's height (DIP): a short one makes the list scroll.
    room: f32,
}

impl<'a> Scene<'a> {
    fn of(text: &'a str, quick: &'a [Hit], files: &'a [Hit], state: FilesState) -> Self {
        Scene { text, quick, files, state, room: 1080.0 }
    }
}

/// The panel in `scene`, put in its state by `set`, to `<name>.png` in
/// both looks at `scales`.
/// shots::on, mirrored across the panel's width when it reads right to
/// left (as Panel::redraw draws it).
unsafe fn on_panel<R>(cv: &kit::Canvas, s: f32, p: &Panel, f: impl FnOnce(&ID2D1DeviceContext) -> R) -> R {
    if !p.rtl {
        return shots::on(cv, s, f);
    }
    let m = kit::HARMONY_M;
    cv.dc.SetTransform(&Matrix3x2 { M11: -s, M12: 0.0, M21: 0.0, M22: s, M31: (m + p.w) * s, M32: m * s });
    let r = f(&cv.dc);
    cv.dc.SetTransform(&Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: 0.0, M32: 0.0 });
    r
}

unsafe fn shoot(name: &str, scene: &Scene, theme_of: fn(bool) -> Theme, scales: &[f32], set: impl Fn(&mut Panel)) {
    let stage = kit::stage();
    register_class();
    for (folder, light) in kit::LOOKS {
        let theme = theme_of(light);
        // DWM's blur stands in as the picture's backdrop (glass::StandIn).
        let _ground = crate::glass::StandIn::new(kit::harmony_backdrop(light));
        for &s in scales {
            // Far off every display (the window is never shown anyway).
            let side = |dip: f32| -32000 + (dip * s) as i32;
            let work = RECT { left: -32000, top: -32000, right: side(1920.0), bottom: side(scene.room) };
            let mut p = Panel::make(&stage.gfx, &theme, s, work, idle).expect("the panel");
            p.rtl = false;
            if !scene.text.is_empty() {
                p.field.insert(scene.text);
                (p.seq, p.quick_seq) = (1, 1);
            }
            p.quick = scene.quick.to_vec();
            p.files = scene.files.to_vec();
            p.files_state = scene.state;
            p.merge(false);
            set(&mut p);
            p.update_scroll();
            // Where the rows are: a first paint tells (as on screen).
            let mut painted = Painted::default();
            let probe = shots::canvas(&stage, &theme, light, p.w, p.height, s, theme.radii().panel, crate::theme::Elevation::E3);
            on_panel(&probe, s, &p, |dc| p.paint(dc, &mut painted)).expect("painted");
            p.painted = painted;
            let cv = shots::canvas(&stage, &theme, light, p.w, p.height, s, theme.radii().panel, crate::theme::Elevation::E3);
            // The fills (the one emphasis, the note's button), cut at the
            // field's foot and fading under it as their layer is (the
            // product's scroll edge: the same band as the rows' picture).
            let m = kit::HARMONY_M;
            let e = p.emphasis();
            let (bs, _) = p.button_states(e);
            let view = kit::rect(m, m + FIELD_H, m + p.w, m + p.height);
            crate::gfx::scroll_fade(&cv.dc, view, banded(p.list_off, theme.high_contrast), false, &theme, 1.0, || {
                if let Some(shape) = e.mark.and_then(|i| p.row_shape(i)) {
                    shots::mark(&cv, &theme, shape);
                }
                if let Some(lit) = e.lit.and_then(|(t, down)| p.row_shape(t).map(|shape| Lit { t, down, shape })) {
                    shots::hover(&cv, &theme, &lit, false);
                }
                if let Some(r) = p.painted.retry {
                    shots::raised(&cv, &theme, Shape::new(r.left, r.top, r.right - r.left, r.bottom - r.top, theme.radii().control), bs);
                }
                Ok(())
            })
            .expect("the fills");
            let mut painted = Painted::default();
            on_panel(&cv, s, &p, |dc| p.paint(dc, &mut painted)).expect("painted");
            if p.ring_on {
                shots::on(&cv, s, |dc| crate::gfx::spinner(dc, p.w - LINE - size::SPINNER / 2.0, FIELD_H / 2.0, 0.0, &theme, 1.0)).expect("the ring");
            }
            shots::save(cv, folder, &shots::file(name, s));
            p.workers.quit(&p.shared);
            let _ = DestroyWindow(p.hwnd);
        }
    }
}

/// The pointer moved onto hit `i`'s row: it is the selection, the
/// pointer's (Panel::point_at).
fn point(p: &mut Panel, i: usize) {
    (p.hover, p.sel, p.by_key) = (Some(i), Some(i), false);
}

/// The pointer moved onto the note's button.
fn on_button(p: &mut Panel) {
    (p.hot_retry, p.sel, p.by_key) = (true, p.note(), false);
}

#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_search() {
    let _l = crate::i18n::test_lock("ko");
    let q = "보고서";
    let web = hit(Kind::Web, &crate::i18n::tf("search.web", &[("q", q)]), "");
    let quick = [hit(Kind::App, "보고서 편집기", ""), hit(Kind::App, "하모니 메모", ""), hit(Kind::Setting, "기본 앱", ""), web.clone()];
    let files = [hit(Kind::File, "분기 보고서.docx", "Z:\\harmony\\문서"), hit(Kind::File, "보고서 초안 2026.txt", "Z:\\harmony\\문서\\초안")];
    let many: Vec<Hit> = (1..=7).map(|k| hit(Kind::File, &format!("보고서 {k}분기.docx"), "Z:\\harmony\\문서")).collect();
    let done = FilesState::Done(FileSource::Index);
    let found = Scene::of(q, &quick, &files, done);
    let two = [kit::scale()];
    unsafe {
        // Text and results, the first row selected.
        shoot("search", &found, shots::plain, &[kit::scale(), 1.0, 1.5], |_| {});
        // Nothing typed: the field alone.
        shoot("search-empty", &Scene::of("", &[], &[], FilesState::Idle), shots::plain, &two, |_| {});
        // The pointer moved onto another row (it is the selection now: the
        // keyboard's fill went); pressing it; the keyboard's cursor moved
        // down to a file (its hint at the row's end).
        shoot("search-hover", &found, shots::plain, &two, |p| point(p, 2));
        shoot("search-pressed", &found, shots::plain, &two, |p| {
            point(p, 2);
            p.pressed = Some(Press::Row(2));
        });
        shoot("search-cursor", &found, shots::plain, &two, |p| p.sel = Some(3));
        // Nothing found: the quiet block over the web row.
        shoot("search-none", &Scene::of(q, &[web.clone()], &[], done), shots::plain, &two, |_| {});
        // Answers late: the ring at the field's end (no row says so).
        shoot("search-slow", &Scene::of(q, &[web.clone()], &[], FilesState::Waiting), shots::plain, &two, |p| p.ring_on = true);
        // The file search went wrong: its note with "Try again" (the
        // pointer on the button); the keyboard's cursor on the note.
        let late = Scene::of(q, &quick, &[], FilesState::TimedOut(FileSource::Index));
        shoot("search-failed", &late, shots::plain, &[kit::scale(), 1.0], |p| on_button(p));
        shoot("search-failed-cursor", &late, shots::plain, &two, |p| p.sel = p.note());
        // A list taller than its room: scrolled under the field (the band
        // under it), the keyboard's cursor near its end.
        let long = Scene { room: 620.0, ..Scene::of(q, &quick, &many, done) };
        shoot("search-scroll", &long, shots::plain, &[kit::scale(), 1.0, 1.5], |p| {
            p.sel = Some(7);
            p.relayout();
        });
        // Scrolled one row and a half: the selection and the pointer's
        // fill both on rows that run into the band under the field (the
        // fills fade there with the rows' text).
        shoot("search-scroll-fills", &long, shots::plain, &[kit::scale(), 1.5], |p| {
            p.scroll_list(ROW_H * 1.5 + 4.0);
            p.sel = Some(1);
        });
        // Glass; high contrast (the pointer on a row: its outline; the
        // selected row inverted; the note's button pressed).
        shoot("search-glass", &found, shots::glass, &[1.5], |p| point(p, 2));
        shoot("search-hc", &found, shots::hc, &[1.5], |p| point(p, 2));
        shoot("search-hc-failed", &late, shots::hc, &[1.5], |p| {
            on_button(p);
            p.pressed = Some(Press::Retry);
        });
        // The keyboard's cursor on the note: its button on the Highlight
        // (HighlightText, its line HighlightText).
        shoot("search-hc-failed-cursor", &late, shots::hc, &[1.5], |p| p.sel = p.note());
        shoot("search-hc-scroll", &long, shots::hc, &[1.5], |p| {
            p.sel = Some(7);
            p.relayout();
        });
    }
}

/// The states inventory (final/states-inventory.md, 2026-11-11): names
/// and folders too long for a row (cut with "…", the hint kept), and right
/// to left (Arabic: the field's glass and the rows' icons at the right,
/// the keyboard's cursor on a file).
#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_search_values() {
    let _l = crate::i18n::test_lock("ko");
    let q = "보고서";
    let quick = [hit(Kind::App, "아주 긴 이름의 보고서 편집기 프로그램 2026 프로페셔널 에디션", ""), hit(Kind::Setting, "기본 앱", "")];
    let files = [hit(Kind::File, "2026년 11월 분기 보고서 최종본의 최종본 진짜 최종 수정 완료.docx", r"Z:\harmony\문서\아주\깊은\폴더\안의\또\다른\폴더")];
    let done = FilesState::Done(FileSource::Index);
    let long = Scene::of(q, &quick, &files, done);
    let two = [kit::scale()];
    unsafe {
        shoot("search-long", &long, shots::plain, &two, |p| p.sel = Some(2));
    }
    drop(_l);
    let _l = crate::i18n::test_lock("ar");
    let quick = [hit(Kind::App, "보고서 편집기", ""), hit(Kind::Setting, "기본 앱", "")];
    let files = [hit(Kind::File, "분기 보고서.docx", r"Z:\harmony\문서")];
    let found = Scene::of(q, &quick, &files, done);
    unsafe {
        shoot("search-rtl", &found, shots::plain, &two, |p| {
            p.rtl = true;
            p.sel = Some(2);
            p.relayout();
        });
    }
}

/// The panels' review (2026-11-11, P2): the rows' fills did not fade under
/// the field with the rows' picture (their layer was only cut at the
/// field's foot). The product's own fills layer is read back off screen
/// (popup's chrome_harmony::read_back: no window shown), scrolled so the
/// selected row runs into the band: its fill's alpha climbs across the
/// band as the picture's does (gfx::scroll_fade), and is whole under it;
/// at the top of the list there is no band.
#[test]
#[ignore = "reads the compositor back: cargo test -p dock harmony_ -- --ignored"]
fn harmony_search_fills_fade_under_the_field() {
    use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
    let _l = crate::i18n::test_lock("ko");
    let stage = kit::stage();
    let c = &stage.gfx.compositor;
    let quick = [hit(Kind::App, "a", ""), hit(Kind::App, "b", ""), hit(Kind::Setting, "c", ""), hit(Kind::Web, "w", "")];
    let files: Vec<Hit> = (1..=7).map(|k| hit(Kind::File, &format!("f{k}"), "Z:\\harmony")).collect();
    let s = 2.0;
    for light in [false, true] {
        let theme = shots::plain(light);
        unsafe {
            register_class();
            let side = |dip: f32| -32000 + (dip * s) as i32;
            let work = RECT { left: -32000, top: -32000, right: side(1920.0), bottom: side(620.0) };
            let mut p = Panel::make(&stage.gfx, &theme, s, work, idle).expect("the panel");
            p.rtl = false;
            p.field.insert("q");
            (p.seq, p.quick_seq) = (1, 1);
            p.quick = quick.to_vec();
            p.files = files.clone();
            p.files_state = FilesState::Done(FileSource::Index);
            p.merge(false);
            p.redraw();
            let (w_px, h_px) = ((p.w * s).ceil() as i32, (p.height * s).ceil() as i32);
            let interop: ICompositorDesktopInterop = c.cast().unwrap();
            let target = interop.CreateDesktopWindowTarget(stage.hwnd, false).unwrap();
            let root = c.CreateContainerVisual().unwrap();
            root.SetSize(Vector2 { X: w_px as f32, Y: h_px as f32 }).unwrap();
            target.SetRoot(&root).unwrap();
            let holder = c.CreateContainerVisual().unwrap();
            holder.SetScale(Vector3 { X: s, Y: s, Z: 1.0 }).unwrap();
            root.Children().unwrap().InsertAtTop(&holder).unwrap();
            let fills = p.edges.as_ref().expect("the fills' scroll edge").stage.clone();
            if let Ok(parent) = fills.Parent() {
                parent.Children().unwrap().Remove(&fills).unwrap();
            }
            holder.Children().unwrap().InsertAtTop(&fills).unwrap();
            // The selection (the keyboard's) on a row whose top is 4 DIP
            // above the field's foot (it runs under the field), then at
            // the list's top.
            let most = p.most();
            let (i, (y, h)) = p.visible().into_iter().filter_map(|i| p.line_of(i).map(|l| (i, l))).find(|&(_, (y, _))| y > FIELD_H + 20.0 && y - FIELD_H + 4.0 <= most).expect("a row that can be scrolled there");
            for (off, band) in [(y - FIELD_H + 4.0, true), (0.0, false)] {
                p.sel = Some(i);
                p.by_key = true;
                p.scroll_list(off);
                assert_eq!(p.list_off, off);
                p.redraw();
                assert_eq!(p.banded, band);
                let px = chrome_harmony_read(&root, w_px, h_px).expect("read back");
                // Alpha down the row's middle column, per DIP from its top.
                let x = (p.w / 2.0 * s) as i32;
                let a = |dip: f32| px[(((dip * s) as i32).clamp(0, h_px - 1) * w_px + x) as usize * 4 + 3] as i32;
                let top = y - off;
                let full = a(top + h / 2.0);
                assert!(full > 8, "{light}: the fill under the band: {full}");
                if band {
                    let (in1, in2) = (a(FIELD_H + 1.0), a(FIELD_H + 8.0));
                    assert!(in1 * 4 < full && in1 < in2 && in2 < full, "{light}: across the band {in1} < {in2} < {full}");
                    assert_eq!(a(FIELD_H + size::SCROLL_FADE + 2.0), full, "{light}: whole under it");
                } else {
                    assert_eq!(a(top + 2.0), full, "{light}: no band at the top");
                }
            }
            // The confirm review (2026-11-11, P2): the bottom band is
            // never on, and the selected row running into it (its foot 4
            // DIP over the panel's) is as strong there as in its middle
            // (it was shown twice there: stronger).
            let (i, off, (y, h)) = p
                .visible()
                .into_iter()
                .filter_map(|i| p.line_of(i).map(|l| (i, l.0 + l.1 - p.height + 4.0, l)))
                .find(|&(_, off, (_, h))| off >= 0.0 && off <= most && h > size::SCROLL_FADE + 8.0)
                .expect("a row that can be scrolled onto the bottom band");
            p.sel = Some(i);
            p.by_key = true;
            p.scroll_list(off);
            p.redraw();
            let px = chrome_harmony_read(&root, w_px, h_px).expect("read back");
            let x = (p.w / 2.0 * s) as i32;
            let a = |dip: f32| px[(((dip * s) as i32).clamp(0, h_px - 1) * w_px + x) as usize * 4 + 3] as i32;
            let top = y - off;
            let full = a(top + h / 2.0);
            assert!(full > 8, "{light}: the fill: {full}");
            for dip in [p.height - 6.0, p.height - 8.0, p.height - size::SCROLL_FADE + 1.0] {
                assert_eq!(a(dip), full, "{light}: the bottom band, {} DIP up: the middle's alpha", p.height - dip);
            }
            drop(target);
            p.workers.quit(&p.shared);
            let _ = DestroyWindow(p.hwnd);
        }
    }
}

unsafe fn chrome_harmony_read(root: &windows::UI::Composition::ContainerVisual, w: i32, h: i32) -> Option<Vec<u8>> {
    crate::popup::chrome_harmony::read_back(root, w, h)
}
