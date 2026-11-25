//! Every page drawn for real (the window's own App and paint, on a hidden
//! window and a software device), what it drew logged (layout.rs) and
//! checked against the spacing and alignment rules: in English, Korean and
//! the longest languages (de, ru, fr), at the default and the least width.

use std::cell::RefCell;

use windows::core::{w, Interface};
use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{D2D1CreateFactory, ID2D1DeviceContext, ID2D1Factory1, D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1, D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_WARP;
use windows::Win32::Graphics::Direct3D11::{D3D11CreateDevice, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::{CreateDispatcherQueueController, DispatcherQueueOptions, RoInitialize, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT, RO_INIT_SINGLETHREADED};
use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassExW, WNDCLASSEXW, WS_EX_NOREDIRECTIONBITMAP, WS_OVERLAPPED};

use super::layout::{self, Item, Kind};
use super::{client, model, App, Hit, SIZE};

unsafe extern "system" fn proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    DefWindowProcW(h, m, w, l)
}

thread_local! {
    /// One WARP device context per target size on a thread: made once,
    /// every page drawn on it after (each draw clears it first).
    static DCS: RefCell<Vec<((u32, u32), ID2D1DeviceContext)>> = const { RefCell::new(Vec::new()) };
}

/// A WARP device context drawing into a `w` x `h` bitmap (DIPs at 96):
/// this thread's, made on first use (review P2: a device per draw made the
/// tests slow).
fn warp_dc(w: u32, h: u32) -> ID2D1DeviceContext {
    if let Some(dc) = DCS.with(|d| d.borrow().iter().find(|(s, _)| *s == (w, h)).map(|(_, dc)| dc.clone())) {
        return dc;
    }
    let dc = new_warp_dc(w, h);
    DCS.with(|d| d.borrow_mut().push(((w, h), dc.clone())));
    dc
}

fn new_warp_dc(w: u32, h: u32) -> ID2D1DeviceContext {
    unsafe {
        let mut dev = None;
        D3D11CreateDevice(None, D3D_DRIVER_TYPE_WARP, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut dev), None, None).unwrap();
        let dxgi: IDXGIDevice = dev.unwrap().cast().unwrap();
        let f: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).unwrap();
        let dc = f.CreateDevice(&dxgi).unwrap().CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE).unwrap();
        let pf = D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED };
        let props = D2D1_BITMAP_PROPERTIES1 { pixelFormat: pf, dpiX: 96.0, dpiY: 96.0, bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET, ..Default::default() };
        let target = dc.CreateBitmap(D2D_SIZE_U { width: w, height: h }, None, 0, &props).unwrap();
        dc.SetTarget(&target);
        dc
    }
}

/// A settings App on a hidden window, offline, with the default settings.
pub(super) struct Harness {
    pub app: App,
    _queue: windows::System::DispatcherQueueController,
    hwnd: HWND,
}

impl Drop for Harness {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

pub(super) fn harness(welcome: bool) -> Harness {
    crate::gfx::set_render_choice("software");
    unsafe {
        let _ = RoInitialize(RO_INIT_SINGLETHREADED);
        let queue = CreateDispatcherQueueController(DispatcherQueueOptions { dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32, threadType: DQTYPE_THREAD_CURRENT, apartmentType: DQTAT_COM_STA }).unwrap();
        let instance = GetModuleHandleW(None).unwrap();
        let wc = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(proc), hInstance: instance.into(), lpszClassName: w!("NopBarSettingsLayoutTest"), ..Default::default() };
        let _ = RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(WS_EX_NOREDIRECTIONBITMAP, w!("NopBarSettingsLayoutTest"), w!(""), WS_OVERLAPPED, 0, 0, 100, 100, None, None, Some(instance.into()), None).unwrap();
        let sys = crate::syslook::current();
        let cfg = crate::config::Config::default();
        let theme = super::settings_look(cfg.load_theme(&crate::config::app_dir()), &sys, &cfg);
        let light = theme.is_light;
        let gfx = crate::gfx::Gfx::new().unwrap();
        let mut app = App::assemble(hwnd, gfx, client::Client::offline(), cfg.clone(), &cfg, sys, 1.0, &theme, light, true, welcome).unwrap();
        // Two displays (the Dock page's map shows then).
        let d = |n: u32, x: i32, primary: bool| crate::displays::Display {
            handle: n as isize,
            rect: windows::Win32::Foundation::RECT { left: x, top: 0, right: x + 1920, bottom: 1080 },
            work: windows::Win32::Foundation::RECT { left: x, top: 0, right: x + 1920, bottom: 1040 },
            primary,
            number: n,
            key: format!("TEST{n}"),
            name: format!("Display {n}"),
        };
        app.screens = vec![d(1, 0, true), d(2, 1920, false)];
        Harness { app, _queue: queue, hwnd }
    }
}

/// Draws page `section` (or the first-run flow's step) at `w` DIPs wide,
/// tall enough for all of it; returns what it drew in the page's content
/// (the sidebar and the title left out), the content's top and its end.
pub(super) fn draw(app: &mut App, section: usize, w: f32) -> (Vec<Item>, f32, f32) {
    let (items, top, end, _) = draw_hits(app, section, w);
    (items, top, end)
}

/// draw, with the page's hits too.
fn draw_hits(app: &mut App, section: usize, w: f32) -> (Vec<Item>, f32, f32, Vec<(D2D_RECT_F, Hit)>) {
    const H: f32 = 4000.0;
    SIZE.with(|s| s.set((w, H)));
    app.section = section;
    app.scroll = 0.0;
    app.page_anim = None;
    app.flow.set(layout::Block::Start);
    *app.log.borrow_mut() = Some(Vec::new());
    let dc = warp_dc(w as u32, H as u32);
    let mut hits: Vec<(D2D_RECT_F, Hit)> = Vec::new();
    let content_h = unsafe {
        dc.BeginDraw();
        dc.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
        let h = app.paint(&dc, &mut hits).unwrap();
        dc.EndDraw(None, None).unwrap();
        h
    };
    let items = app.log.borrow_mut().take().unwrap_or_default();
    let (top, left, bottom) = match &app.welcome {
        // The step's body (the bar under it is the flow's own).
        Some(w) => (w.body.get().0, 0.0, w.body.get().0 + w.body.get().1),
        None => (app.content_top(), super::SIDEBAR, f32::MAX),
    };
    let _ = left;
    // (An "i" by its middle, its glyph: its 24 DIP target may reach over
    // the content's top, as layout::check places it.)
    let items = items
        .into_iter()
        .filter(|i| {
            let (t, b) = if i.kind == Kind::Info { let c = (i.r.top + i.r.bottom) / 2.0; (c, c) } else { (i.r.top, i.r.bottom) };
            t >= top - 0.5 && b <= bottom
        })
        .collect();
    (items, top, top + content_h, hits)
}

/// Every list filled and every fold open: a dock on every display, pins,
/// calendar links (one being added) and input rules;
/// Win+digit on and two shortcuts on the same keys.
pub(super) fn fill(app: &mut App) {
    use crate::config::{CalendarFeed, InputRule};
    app.cfg.dock.set_display_mode(2);
    app.display_sel = 1;
    app.cfg.calendar.feeds = vec![CalendarFeed { name: "Work".into(), url: "https://example.com/a.ics".into() }, CalendarFeed { name: String::new(), url: "https://example.org/b.ics".into() }];
    app.cal.adding = true;
    app.cfg.input.rules = vec![InputRule { app: "name:code.exe".into(), name: "Visual Studio Code".into(), mode: "alpha".into() }];
    let folds: Vec<&'static str> = app.sections.iter().flat_map(|s| s.rows.iter()).filter(|r| matches!(r.ctl, model::Ctl::Fold { .. })).map(|r| r.key).collect();
    app.folds = folds;
    // Win+digit on (its hook: said behind the row's "i"), and a shortcut
    // on another's keys (a note under both rows).
    app.cfg.set_shortcut(crate::shortcuts::DOCK_APPS, "win");
    app.cfg.set_shortcut(crate::shortcuts::SEARCH, "alt+space");
    app.cfg.set_shortcut(crate::shortcuts::WINDOW_TO_TRAY, "alt+space");
}

thread_local! {
    static FOUND: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Every page in `lang` at `w`: what is wrong (page title: problem).
fn pages_in(h: &mut Harness, lang: &str, w: f32) -> Vec<String> {
    let _l = crate::i18n::test_lock(lang);
    let mut bad = Vec::new();
    for i in 0..h.app.sections.len() {
        let (items, top, end) = draw(&mut h.app, i, w);
        let page = format!("{lang} {w} {}", crate::i18n::t(h.app.sections[i].title));
        bad.extend(layout::check(&page, &items, top, end, crate::i18n::rtl()));
    }
    bad
}

#[test]
fn every_page_follows_the_spacing_and_alignment_rules() {
    let mut h = harness(false);
    let mut bad = Vec::new();
    for (lang, w) in [("en", 820.0), ("ko", 820.0), ("de", 720.0), ("ru", 720.0), ("fr", 720.0)] {
        bad.extend(pages_in(&mut h, lang, w));
    }
    // Again with every list filled and every fold open.
    fill(&mut h.app);
    for (lang, w) in [("en", 820.0), ("ko", 820.0), ("de", 720.0), ("ar", 820.0)] {
        bad.extend(pages_in(&mut h, lang, w).into_iter().map(|b| format!("(filled) {b}")));
    }
    FOUND.with(|f| f.borrow_mut().clone_from(&bad));
    assert!(bad.is_empty(), "{} problems:\n{}", bad.len(), bad.join("\n"));
}

#[test]
fn companion_ai_connected_cards_expose_only_active_provider_controls() {
    let _detection=crate::companion::chat::cli::test_detection(crate::companion::chat::cli::Status::Ready);
    use crate::companion::chat::state::Provider;
    let mut h=harness(false);
    let _lang=crate::i18n::test_lock("ko");
    let section=h.app.sections.iter().position(|s|s.tab==model::Tab::CompanionAi).unwrap();
    for provider in [Provider::ClaudeCode,Provider::Antigravity] {
        h.app.cfg.chat.activate(provider);
        let (items,top,end,hits)=draw_hits(&mut h.app,section,720.);
        let bad=layout::check("linked AI",&items,top,end,false);
        assert!(bad.is_empty(),"{}",bad.join("\n"));
        for action in ["chat.disconnect","chat.model","chat.limit.minus","chat.limit.plus"]{assert!(hits.iter().any(|(_,h)|*h==Hit::Chat(action)));}
        assert!(!hits.iter().any(|(_,h)|*h==Hit::Chat("chat.login")));
        assert!(!hits.iter().any(|(_,h)|*h==Hit::Chat("chat.account")));
        h.app.cfg.chat.disconnect_cli();
        let (_,_,_,hits)=draw_hits(&mut h.app,section,720.);
        assert!(hits.iter().any(|(_,h)|*h==Hit::Chat("chat.login")));
        for action in ["chat.disconnect","chat.model","chat.limit.minus","chat.limit.plus"]{assert!(!hits.iter().any(|(_,h)|*h==Hit::Chat(action)));}
    }
}

#[test]
fn notification_navigation_and_missing_tool_hits() {
    let _lang = crate::i18n::test_lock("ko");
    let _detection = crate::companion::chat::cli::test_detection(crate::companion::chat::cli::Status::Missing);
    let mut h = harness(false);
    let ai = h.app.sections.iter().position(|s| s.tab == model::Tab::CompanionAi).unwrap();
    let agents = h.app.sections.iter().position(|s| s.tab == model::Tab::Agents).unwrap();
    h.app.section = ai;
    h.app.click(Hit::Chat("chat.ai.agents"), 0.);
    assert_eq!(h.app.section, agents);
    for query in ["에이전트", "Claude Code", "Codex", "Antigravity", "절전", "말풍선"] {
        h.app.search = query.into();
        let index = h.app.results().iter().position(|f| f.section == agents).unwrap();
        h.app.click(Hit::SearchResult(index), 0.);
        assert_eq!(h.app.section, agents, "{query}");
    }
    h.app.search.clear();
    h.app.filtering = false;
    let (_, _, _, hits) = draw_hits(&mut h.app, agents, 820.);
    for key in ["chat.claude_mcp", "chat.antigravity_mcp"] {
        let ri = h.app.rows().iter().position(|r| r.key == key).unwrap();
        assert!(!hits.iter().any(|(_, hit)| *hit == Hit::Toggle(ri)), "{key}");
    }
}

#[test]
fn companion_ai_checking_and_deleted_cli_show_recovery_without_false_connection() {
    use crate::companion::chat::{cli::{self,Status},state::Provider};
    let mut h=harness(false);
    let _lang=crate::i18n::test_lock("ko");
    let section=h.app.sections.iter().position(|s|s.tab==model::Tab::CompanionAi).unwrap();
    {
        let _detection=cli::test_detection(Status::Checking);
        let (_,_,_,hits)=draw_hits(&mut h.app,section,720.);
        for action in ["chat.use.claude","chat.use.agy"] {assert!(!hits.iter().any(|(_,h)|*h==Hit::Chat(action)));}
    }
    h.app.cfg.chat.activate(Provider::ClaudeCode);
    let _detection=cli::test_detection(Status::Missing);
    let (_,_,_,hits)=draw_hits(&mut h.app,section,720.);
    assert!(hits.iter().any(|(_,h)|*h==Hit::Chat("chat.restore")));
    assert!(!hits.iter().any(|(_,h)|*h==Hit::Chat("chat.model")));
    assert!(!super::companion_ai::connected(&h.app.cfg.chat,false));
    assert!(!cli::awake(Provider::ClaudeCode));
}

#[test]
fn official_provider_svgs_render_at_requested_sizes_without_fallback() {
    let mut h=harness(false);
    let dc=warp_dc(128,128);
    for light in [false,true] {
        h.app.light=light;
        for name in ["ChatGPT","Claude Code","Antigravity"] {
            for size in [18.,24.,28.] {unsafe {
                dc.BeginDraw();
                h.app.provider_logo(&dc,name,8.,8.,size).unwrap();
                dc.EndDraw(None,None).unwrap();
            }}
        }
    }
}

#[test]
fn the_first_run_steps_follow_them_too() {
    let mut h = harness(true);
    let mut bad = Vec::new();
    for lang in ["en", "ko", "de"] {
        let _l = crate::i18n::test_lock(lang);
        for step in 0..super::welcome::STEPS {
            if let Some(w) = h.app.welcome.as_mut() {
                w.flow.step = step;
                w.rows = model::welcome_rows(step);
            }
            let (items, top, end) = draw(&mut h.app, 0, 820.0);
            // The step's own pictures (the taskbar's and the themes') are
            // cards; its rows, the pages' rows.
            bad.extend(layout::check(&format!("{lang} welcome {step}"), &items, top, end, crate::i18n::rtl()));
        }
    }
    assert!(bad.is_empty(), "{} problems:\n{}", bad.len(), bad.join("\n"));
}

#[test]
fn no_page_draws_a_row_s_description() {
    // Labels only: a row's description is behind its "i" (and its help);
    // only the few names that mean nothing alone (model::SUBTITLED) have
    // theirs under the label.
    let mut h = harness(false);
    for lang in ["en", "ko"] {
        let _l = crate::i18n::test_lock(lang);
        let hints: std::collections::HashSet<&str> = model::sections().iter().flat_map(|s| s.rows.iter()).filter(|r| !r.hint.is_empty() && !model::subtitled(r)).map(|r| crate::i18n::t(r.hint)).collect();
        for i in 0..h.app.sections.len() {
            let (items, ..) = draw(&mut h.app, i, 820.0);
            for it in items.iter().filter(|i| i.kind == Kind::Text) {
                assert!(!hints.contains(it.key.as_str()), "{lang} {}: shows {:?}", h.app.sections[i].title, it.key);
            }
        }
    }
}

/// The widgets card: each widget's grey line under its name (with its
/// shortcut), no "i" on those rows; a row under a switch that is off is
/// not drawn and not reachable (its value kept); the load's word on the
/// heading only while a widget is on.
#[test]
fn the_widgets_card_says_what_each_widget_is() {
    use crate::desklayer::load::{status, Kind as W, Spec, Usage};
    let mut h = harness(false);
    let _l = crate::i18n::test_lock("en");
    let general = h.app.sections.iter().position(|s| s.tab == model::Tab::General).unwrap();
    let at = |app: &App, k: &str| app.rows().iter().position(|r| r.key == k).unwrap();
    h.app.client.widgets = Some(status(&[(W::Notes, Usage::default()), (W::Pieces, Usage::default())], Spec { ram: 16 << 30, cores: 8 }));
    (h.app.cfg.widgets.notes, h.app.cfg.widgets.pieces) = (true, true);
    h.app.cfg.widgets.notes_per_desktop = true;
    let (items, _, _, hits) = draw_hits(&mut h.app, general, 820.0);
    let texts: Vec<&str> = items.iter().filter(|i| i.kind == Kind::Text).map(|i| i.key.as_str()).collect();
    let notes = h.app.row_hint(&h.app.rows()[at(&h.app, "widgets.notes")]).to_string();
    assert!(notes.starts_with(crate::i18n::t("widgets.notes.hint")) && notes.contains("Win+Alt+N"), "what it is and its shortcut: {notes}");
    assert!(!notes.contains('\u{229E}'), "the shortcut in plain text: {notes}");
    for k in ["widgets.notes", "widgets.pieces"] {
        let line = h.app.row_hint(&h.app.rows()[at(&h.app, k)]).to_string();
        assert!(!texts.contains(&line.as_str()), "{k}: explanation is tooltip only");
        assert!(hits.iter().any(|(_, x)| *x == Hit::Info(at(&h.app, k))), "{k}: tooltip");
    }
    assert!(texts.contains(&crate::i18n::t("widgets.load.light")), "the load's word");
    let create = Hit::NewNote(at(&h.app, "widgets.notes"));
    assert!(hits.iter().any(|(_, x)| *x == create));
    assert!(h.app.a11y_key(create).is_some());
    let per_desktop = at(&h.app, "widgets.notes_per_desktop");
    assert!(hits.iter().any(|(_, x)| *x == Hit::Toggle(per_desktop)));
    // The notes off: the row under them gone, its value kept.
    h.app.cfg.widgets.notes = false;
    let (items, _, _, hits) = draw_hits(&mut h.app, general, 820.0);
    assert!(!hits.iter().any(|(_, x)| matches!(x, Hit::NewNote(_))));
    assert!(!hits.iter().any(|(_, x)| super::row_of(*x) == Some(per_desktop)), "hidden: out of reach");
    assert!(!items.iter().any(|i| i.kind == Kind::Text && i.key == crate::i18n::t("widgets.notes_per_desktop")), "hidden: not drawn");
    assert!(h.app.cfg.widgets.notes_per_desktop, "its value kept");
    // Every widget off: no word on the heading.
    h.app.cfg.widgets.pieces = false;
    let (items, ..) = draw(&mut h.app, general, 820.0);
    assert!(!items.iter().any(|i| i.kind == Kind::Text && i.key == crate::i18n::t("widgets.load.light")));
    // Screen sharing: a card of its own, not under the widgets'.
    let slots = h.app.row_slots(h.app.rows());
    assert!(matches!(slots[at(&h.app, "widgets.hide_from_capture")], super::rows::Slot::Row { first: true, last: true, indent: false }));
}

/// Every page's hits as drawn (default settings, English).
fn page_hits(app: &mut App, section: usize) -> Vec<(D2D_RECT_F, Hit)> {
    SIZE.with(|s| s.set((820.0, 4000.0)));
    app.section = section;
    app.scroll = 0.0;
    let dc = warp_dc(820, 4000);
    let mut hits = Vec::new();
    unsafe {
        dc.BeginDraw();
        app.paint(&dc, &mut hits).unwrap();
        dc.EndDraw(None, None).unwrap();
    }
    hits
}

#[test]
fn every_i_is_small_with_a_24_dip_target_and_a_reader_hears_it() {
    let mut h = harness(false);
    fill(&mut h.app);
    let _l = crate::i18n::test_lock("en");
    let mut seen = 0;
    for i in 0..h.app.sections.len() {
        for (r, hit) in page_hits(&mut h.app, i) {
            let Hit::Info(id) = hit else { continue };
            seen += 1;
            assert_eq!((r.right - r.left, r.bottom - r.top), (24.0, 24.0), "{}: {id}", h.app.sections[i].title);
            // The reader hears the same text the tip shows.
            let text = h.app.tip_text(id);
            assert!(!text.trim().is_empty(), "{}: {id} says nothing", h.app.sections[i].title);
            let node = h.app.a11y_node(hit, windows::Win32::Foundation::RECT::default()).expect("an i is a node");
            assert_eq!(node.help, text);
        }
        // Every row with a description: its control still carries it as
        // the help (shown or not).
        for (_, hit) in page_hits(&mut h.app, i) {
            let Some(ri) = super::row_of(hit) else { continue };
            let Some(row) = h.app.rows().get(ri) else { continue };
            if row.hint.is_empty() || matches!(hit, Hit::Info(_) | Hit::ShortcutReset(_) | Hit::RowFix(_) | Hit::MenuItem(..)) {
                continue;
            }
            if let Some(node) = h.app.a11y_node(hit, windows::Win32::Foundation::RECT::default()) {
                assert!(node.help.contains(&*h.app.row_hint(row)), "{}: {} lost its help", h.app.sections[i].title, row.key);
            }
        }
    }
    assert!(seen > 40, "only {seen} i's");
    // The glyph stays small: smaller than the label's text.
    const { assert!(super::rows::tok::INFO_D < super::rows::tok::LABEL_SIZE) };
}

#[test]
fn the_keyboard_on_an_i_shows_its_tip_at_once() {
    let mut h = harness(false);
    let _l = crate::i18n::test_lock("en");
    let general = h.app.sections.iter().position(|s| s.tab == model::Tab::General).unwrap();
    let hits = page_hits(&mut h.app, general);
    let (_, info) = *hits.iter().find(|(_, x)| matches!(x, Hit::Info(_))).expect("General has an i");
    let Hit::Info(id) = info else { unreachable!() };
    h.app.keyboard = true;
    h.app.tip_key = true;
    h.app.focus = Some(info);
    h.app.sync_tip();
    let tip = h.app.tip.expect("a tip for the focused i");
    assert_eq!(tip.ri, id);
    assert!(tip.from <= std::time::Instant::now(), "no wait for the keyboard");
    // Drawn: its text is on the page.
    let (items, ..) = draw(&mut h.app, general, 820.0);
    let _ = items;
    *h.app.log.borrow_mut() = Some(Vec::new());
    let _ = page_hits(&mut h.app, general);
    let drawn = h.app.log.borrow_mut().take().unwrap_or_default();
    assert!(drawn.iter().any(|i| i.kind == Kind::Text && i.key == h.app.tip_text(id)), "the tip's text is drawn");
}

/// Page `section` drawn (`lang` already held) at 820 DIP: its hits, kept
/// as the window keeps them (focusables, the Tab order, read them).
fn drawn_hits(app: &mut App, section: usize) -> Vec<(D2D_RECT_F, Hit)> {
    let hits = page_hits(app, section);
    app.hits = hits.clone();
    hits
}

/// Every row shown with a description has its "i" on the page: a hit
/// for that row (the light / dark pictures' on the heading over them),
/// not only a model that says it should (settings-info review P2).
#[test]
fn every_described_row_shown_has_its_i() {
    let mut h = harness(false);
    fill(&mut h.app);
    let _l = crate::i18n::test_lock("en");
    for i in 0..h.app.sections.len() {
        let hits = drawn_hits(&mut h.app, i);
        let rows = h.app.rows();
        let slots = h.app.row_slots(rows);
        for (ri, row) in rows.iter().enumerate() {
            if !model::info_hint(row) || matches!(slots[ri], super::rows::Slot::Hidden) {
                continue;
            }
            assert!(hits.iter().any(|(_, x)| *x == Hit::Info(ri)), "{}: {} has a description but no i", h.app.sections[i].title, row.key);
        }
    }
    // The theme's pictures: their "i" on the Theme heading.
    let look = h.app.sections.iter().position(|s| s.title == "settings.section.appearance").unwrap();
    let hits = drawn_hits(&mut h.app, look);
    let mode = h.app.rows().iter().position(|r| r.key == "theme.mode").unwrap();
    let (r, _) = hits.iter().find(|(_, x)| *x == Hit::Info(mode)).expect("the pictures' i");
    let first_tile = hits.iter().find(|(_, x)| matches!(x, Hit::Choice(ri, _) if *ri == mode)).map(|(r, _)| *r).expect("the tiles");
    assert!(r.bottom <= first_tile.top + 0.5, "over the pictures");
    assert_eq!(h.app.tip_text(mode), crate::i18n::t(h.app.rows()[mode].hint));
}

/// The hit areas a page may nest (outer, inner; the inner one pushed later
/// wins where it is): Support development's row and its "i"; a key
/// field and its own Reset (Shortcuts page).
fn nests(outer: &Hit, inner: &Hit) -> bool {
    match (outer, inner) {
        (Hit::Shortcut(a), Hit::ShortcutReset(b)) => a == b,
        _ => false,
    }
}

/// What a click reaches: on every page (lists filled, folds open; left to
/// right and right to left), two hit areas overlap only as a pair `nests`
/// allows, the inner one on top; never the same area twice (settings-info
/// review P2, structure review P3).
#[test]
fn no_page_has_hit_areas_that_steal_clicks() {
    let mut h = harness(false);
    let mut bad = Vec::new();
    for filled in [false, true] {
        if filled {
            fill(&mut h.app);
        }
        for lang in ["en", "de", "ar"] {
            let _l = crate::i18n::test_lock(lang);
            for i in 0..h.app.sections.len() {
                let hits = page_hits(&mut h.app, i);
                let page = format!("{lang} {}{}", h.app.sections[i].title, if filled { " (filled)" } else { "" });
                bad.extend(layout::hit_conflicts(&hits, nests).into_iter().map(|b| format!("[{page}] {b}")));
            }
        }
    }
    assert!(bad.is_empty(), "{} problems:\n{}", bad.len(), bad.join("\n"));
    // Only those pairs: another row's Reset, an "i" in a row that is
    // not Support development's, are not.
    assert!(!nests(&Hit::Shortcut(0), &Hit::ShortcutReset(1)) && !nests(&Hit::Link(0), &Hit::Info(1)));
}

/// The Tab order reads like the page: an "i" comes before what follows it
/// on its row in reading order (its row's button too: Report a problem,
/// the display map's Identify), left to right and right to left
/// (settings-info review P3).
#[test]
fn tab_reaches_a_rows_i_before_its_control() {
    let mut h = harness(false);
    fill(&mut h.app);
    for lang in ["en", "ar"] {
        let _l = crate::i18n::test_lock(lang);
        let rtl = crate::i18n::rtl();
        for i in 0..h.app.sections.len() {
            let hits = drawn_hits(&mut h.app, i);
            let order = h.app.focusables();
            let at = |x: Hit| order.iter().position(|o| super::same(*o, x));
            for (ir, info) in hits.iter().filter(|(_, x)| matches!(x, Hit::Info(_))) {
                let cy = (ir.top + ir.bottom) / 2.0;
                let Some(ii) = at(*info) else { continue };
                for (r, x) in &hits {
                    let on_row = r.top <= cy && r.bottom >= cy && r.bottom - r.top < 80.0;
                    let after = if rtl { r.right <= ir.left + 0.5 } else { r.left >= ir.right - 0.5 };
                    if !on_row || !after || matches!(x, Hit::Info(_) | Hit::Section(_)) {
                        continue;
                    }
                    if let Some(xi) = at(*x) {
                        assert!(ii < xi, "{lang} {}: {x:?} comes before the i {info:?} on its row", h.app.sections[i].title);
                    }
                }
            }
        }
    }
    // The pure helper: the "i" goes before the button pushed first.
    let r = |x: f32| D2D_RECT_F { left: x, top: 0.0, right: x + 10.0, bottom: 10.0 };
    let mut v = vec![(r(0.0), Hit::Section(0)), (r(90.0), Hit::ReportProblem), (r(20.0), Hit::Info(7))];
    super::cards::info_first(&mut v, 1);
    assert_eq!(v.iter().map(|(_, x)| *x).collect::<Vec<_>>(), [Hit::Section(0), Hit::Info(7), Hit::ReportProblem]);
}

/// "Add calendar…" opens its field; Esc closes it and the keyboard is
/// back on "Add calendar…", so Tab and Shift+Tab go on from there, not
/// from the page's first control (settings-info review P3).
#[test]
fn esc_in_the_calendar_field_returns_to_add_calendar() {
    let mut h = harness(false);
    let _l = crate::i18n::test_lock("en");
    let clock = h.app.sections.iter().position(|s| s.kind == model::Kind::Calendar).unwrap();
    h.app.cfg.calendar.feeds = vec![crate::config::CalendarFeed { name: "Work".into(), url: "https://example.com/a.ics".into() }];
    h.app.keyboard = true;
    drawn_hits(&mut h.app, clock);
    h.app.click(Hit::CalOpen, 0.0);
    assert_eq!(h.app.focus, Some(Hit::CalUrl), "the field takes the keyboard");
    assert_eq!(h.app.cal_key(0x1B), Some(true));
    assert!(!h.app.cal.adding && h.app.cal.url.is_empty());
    assert_eq!(h.app.focus, Some(Hit::CalOpen));
    drawn_hits(&mut h.app, clock);
    let list = h.app.focusables();
    let pos = list.iter().position(|x| *x == Hit::CalOpen).expect("Add calendar… is drawn again");
    assert!(pos > 0, "not the page's first control");
    h.app.tab_step(&list, Some(pos), false);
    assert_eq!(h.app.focus, Some(list[pos - 1]), "Shift+Tab: the one before it");
    h.app.focus = Some(Hit::CalOpen);
    h.app.tab_step(&list, Some(pos), true);
    assert_eq!(h.app.focus, Some(list[(pos + 1) % list.len()]), "Tab: the one after it");
    // Renaming: Esc goes back to that calendar's Rename.
    h.app.click(Hit::CalRename(0), 0.0);
    assert_eq!(h.app.cal_key(0x1B), Some(true));
    assert_eq!(h.app.focus, Some(Hit::CalRename(0)));
    assert_eq!(h.app.cfg.calendar.feeds[0].name, "Work", "the name it had");
}

/// The three tabs with sub-pages (Dock: Dock / Pinned apps, Top bar:
/// Top bar / Clock, Keyboard: Shortcuts / Input): their switch's names fit
/// whole in every language at the smallest window (paint_subtabs' sizes:
/// the body size, else the footnote's), no ellipsis.
#[test]
fn sub_page_names_fit_whole_in_every_language() {
    let h = harness(false);
    let room = super::MIN_W - super::SIDEBAR - 2.0 * super::PAD - 4.0;
    let multi: Vec<model::Tab> = model::Tab::ALL.iter().copied().filter(|t| model::pages_of(&h.app.sections, *t).len() > 1).collect();
    assert_eq!(multi.len(), 3);
    for (tag, _) in crate::i18n::languages() {
        let _l = crate::i18n::test_lock(tag);
        for tab in &multi {
            let names: Vec<&str> = model::pages_of(&h.app.sections, *tab).into_iter().map(|i| crate::i18n::t(h.app.sections[i].title)).collect();
            let widths = |size: f32| names.iter().map(|n| h.app.text_w(n, size, true)).collect::<Vec<f32>>();
            let mut text = widths(super::rows::tok::BODY);
            if text.iter().sum::<f32>() + 12.0 * names.len() as f32 > room {
                text = widths(super::rows::tok::FOOTNOTE);
            }
            let (fit, _) = super::fit_subtabs(&text, room);
            assert!(fit.iter().zip(&text).all(|(f, t)| *f >= t - 0.5), "{tag} {tab:?}: {names:?} cut in {room}");
        }
    }
}

/// The notes under rows are live status only (a clash, a refusal,
/// something turned off); what a setting also does is behind its "i"
/// (owner, 0.1.46: "while on, NopBar watches the keyboard..." drawn under
/// Switch to dock apps by number).
#[test]
fn notes_under_rows_are_status_only() {
    let mut h = harness(false);
    fill(&mut h.app);
    let keys = h.app.sections.iter().position(|s| s.kind == model::Kind::Shortcuts).unwrap();
    // Every page in English; the Shortcuts page (the hook's) in Korean.
    for (lang, pages) in [("en", (0..h.app.sections.len()).collect::<Vec<_>>()), ("ko", vec![keys])] {
        let _l = crate::i18n::test_lock(lang);
        let hook = crate::i18n::t("shortcuts.note.hook");
        let mut notes = 0;
        for i in pages {
            let (items, ..) = draw(&mut h.app, i, 820.0);
            let tips: Vec<String> = (0..h.app.rows().len()).map(|ri| h.app.tip_text(ri)).filter(|t| !t.is_empty()).collect();
            for n in items.iter().filter(|n| n.kind == Kind::Note) {
                notes += 1;
                assert_ne!(n.key, hook, "{lang}: the hook's sentence drawn under its row");
                assert!(!tips.iter().any(|t| t.contains(&n.key)), "{lang} {}: {:?} is a description, drawn under a row", h.app.sections[i].title, n.key);
            }
            for it in items.iter().filter(|t| t.kind == Kind::Text) {
                assert_ne!(it.key, hook, "{lang}: the hook's sentence drawn");
            }
        }
        assert!(notes >= 2, "{lang}: the clashing shortcuts' notes are drawn ({notes})");
        // Behind the row's "i" instead (its tip, a reader's help).
        h.app.section = keys;
        let ri = h.app.rows().iter().position(|r| r.key == crate::hotkeys::DOCK_APPS).expect("the row");
        assert!(h.app.tip_text(ri).contains(hook), "{lang}: {:?}", h.app.tip_text(ri));
        // Off: the row's own description alone.
        h.app.cfg.set_shortcut(crate::shortcuts::DOCK_APPS, "none");
        assert!(!h.app.tip_text(ri).contains(hook));
        h.app.cfg.set_shortcut(crate::shortcuts::DOCK_APPS, "win");
    }
}

/// A note with its fix under it ("Start with Windows" that Windows is not
/// doing, its "Open Startup apps" link): the row's padding is even from
/// the link's end, not from the note's text (review P3), as for a note
/// with no link (a clash).
#[test]
fn a_note_with_a_link_keeps_the_row_s_padding_even() {
    let mut h = harness(false);
    let _l = crate::i18n::test_lock("en");
    let general = h.app.sections.iter().position(|s| s.tab == model::Tab::General).unwrap();
    for state in [(true, false), (false, true)] {
        h.app.startup = Some(state);
        let (items, top, end) = draw(&mut h.app, general, 820.0);
        let note = items.iter().find(|n| n.kind == Kind::Note).expect("the startup note");
        assert!(note.key == crate::i18n::t("settings.startup.not_on") || note.key == crate::i18n::t("settings.startup.not_off"), "{:?}", note.key);
        assert_eq!(layout::check(&format!("startup {state:?}"), &items, top, end, false), Vec::<String>::new());
    }
    h.app.startup = None;
}

/// The keyboard on an "i": its ring is drawn (a ring for that very "i",
/// found by its id, not by where it is: review P3) round its glyph, on
/// every page, left to right and right to left. The first and the last
/// "i" of each page (a heading's and a row's, a card's): the ring's size
/// and place are focus_ring_rect's, tested pure on every kind.
#[test]
fn every_i_s_focus_ring_hugs_its_glyph() {
    let mut h = harness(false);
    fill(&mut h.app);
    let mut bad = Vec::new();
    let mut rings = 0;
    for lang in ["en", "ar"] {
        let _l = crate::i18n::test_lock(lang);
        for i in 0..h.app.sections.len() {
            h.app.keyboard = false;
            h.app.focus = None;
            let (.., hits) = draw_hits(&mut h.app, i, 820.0);
            // This matrix audits body layout; title controls are checked separately.
            let infos: Vec<Hit> = hits.iter().filter(|(r, _)| (r.top + r.bottom) / 2. >= h.app.content_top()).map(|(_, x)| *x).filter(|x| matches!(x, Hit::Info(_))).collect();
            let (Some(first), Some(last)) = (infos.first(), infos.last()) else { continue };
            let mut picked = vec![*first];
            if last != first {
                picked.push(*last);
            }
            for info in picked {
                h.app.keyboard = true;
                h.app.focus = Some(info);
                let (items, top, end) = draw(&mut h.app, i, 820.0);
                let page = format!("{lang} {} {info:?}", crate::i18n::t(h.app.sections[i].title));
                let key = format!("{info:?}");
                let n = items.iter().filter(|r| r.kind == Kind::Ring && r.key == key).count();
                if n != 1 {
                    bad.push(format!("[{page}] {n} rings for the focused {key}"));
                }
                rings += n;
                bad.extend(layout::check(&page, &items, top, end, crate::i18n::rtl()));
            }
        }
    }
    h.app.keyboard = false;
    h.app.focus = None;
    assert!(rings > 20, "only {rings} rings drawn");
    assert!(bad.is_empty(), "{} problems:\n{}", bad.len(), bad.join("\n"));
}

#[test]
fn title_privacy_info_keeps_its_hit_tooltip_and_keyboard_ring() {
    let mut h = harness(false);
    let section = h.app.sections.iter().position(|s| s.tab == model::Tab::CompanionAi).unwrap();
    let info = Hit::Info(super::tooltip::card_tip("chat.ai.privacy"));
    for lang in ["en", "ko", "ar"] {
        let _lang = crate::i18n::test_lock(lang);
        h.app.section = section;
        h.app.keyboard = true;
        h.app.focus = Some(info);
        h.app.page_anim = None;
        SIZE.with(|s| s.set((820., 620.)));
        *h.app.log.borrow_mut() = Some(Vec::new());
        let dc = warp_dc(820, 620);
        let mut hits = Vec::new();
        unsafe { dc.BeginDraw(); h.app.paint(&dc, &mut hits).unwrap(); dc.EndDraw(None, None).unwrap(); }
        let target = hits.iter().find(|(_, hit)| *hit == info).unwrap().0;
        assert!(target.bottom < h.app.content_top());
        assert_eq!((target.right-target.left, target.bottom-target.top), (24.,24.));
        let items = h.app.log.borrow_mut().take().unwrap();
        let rings: Vec<_> = items.iter().filter(|i| i.kind == Kind::Ring && i.key == format!("{info:?}")).collect();
        assert_eq!(rings.len(), 1, "{lang}: title info has a keyboard ring");
        assert!(rings[0].r.top >= 0. && rings[0].r.bottom < h.app.content_top());
        assert!(!h.app.tip_text(super::tooltip::card_tip("chat.ai.privacy")).is_empty());
    }
}

/// A widget row's how-to card: the pointer resting on the row waits
/// (howto::HOLD), the keyboard on its switch shows it at once; Esc and a
/// scroll put it away; none for a row that is not a widget's.
#[test]
fn a_widget_row_s_card_comes_with_the_pointer_or_the_keyboard() {
    let mut h = harness(false);
    let _l = crate::i18n::test_lock("en");
    let general = h.app.sections.iter().position(|s| s.tab == model::Tab::General).unwrap();
    let _ = draw(&mut h.app, general, 820.0);
    let at = |app: &App, k: &str| app.rows().iter().position(|r| r.key == k).unwrap();
    let notes = at(&h.app, "widgets.notes");
    let (_, r) = *h.app.howto_rows.borrow().iter().find(|(ri, _)| *ri == notes).expect("the notes' row drawn");
    assert_eq!(h.app.howto_row_at(r.left + 10.0, (r.top + r.bottom) / 2.0), Some(notes));
    // The clock is the test's: no assertion depends on how fast it runs.
    let t0 = std::time::Instant::now();
    h.app.howto_hover = Some(notes);
    h.app.sync_howto_at(t0);
    let t = h.app.howto.expect("waiting");
    assert_eq!((t.ri, t.from), (notes, t0 + super::howto::HOLD), "the pointer waits");
    let later = t0 + std::time::Duration::from_secs(2);
    h.app.sync_howto_at(later);
    assert_eq!(h.app.howto.map(|t| t.from), Some(t0 + super::howto::HOLD), "the same card goes on");
    h.app.quiet_howto();
    h.app.sync_howto_at(later);
    assert!(h.app.howto.is_some_and(|t| t.hide == Some((later, 1.0))), "Esc: it fades out");
    assert!(h.app.howto_moving_at(later));
    let gone = later + std::time::Duration::from_secs(1);
    assert!(!h.app.howto_moving_at(gone), "faded: nothing runs");
    h.app.howto_page_moved();
    assert_eq!(h.app.howto_hover, None, "a scroll: the pointer's row forgotten");
    // The keyboard on a widget's switch: at once.
    h.app.howto_quiet = None;
    h.app.howto = None;
    h.app.keyboard = true;
    h.app.tip_key = true;
    let pieces = at(&h.app, "widgets.pieces");
    h.app.focus = Some(Hit::Toggle(pieces));
    h.app.sync_howto();
    assert!(h.app.howto.is_some_and(|t| t.ri == pieces && t.from <= std::time::Instant::now()));
    // Not a widget's switch: none.
    h.app.howto = None;
    h.app.focus = Some(Hit::Toggle(at(&h.app, "widgets.hide_from_capture")));
    h.app.sync_howto();
    assert!(h.app.howto.is_none());
}

/// A how-to card stops when its row goes (a search narrows the page to
/// other rows, its switch's parent goes off) or the window is not in front
/// (switched away, minimized): it fades or goes, and then nothing of it
/// runs (review P2 #1, #2).
#[test]
fn a_widget_row_s_card_stops_when_its_row_or_the_window_goes() {
    let mut h = harness(false);
    let _l = crate::i18n::test_lock("en");
    let general = h.app.sections.iter().position(|s| s.tab == model::Tab::General).unwrap();
    let _ = draw(&mut h.app, general, 820.0);
    let notes = h.app.rows().iter().position(|r| r.key == "widgets.notes").unwrap();
    let t0 = std::time::Instant::now();
    let up = t0 + std::time::Duration::from_secs(2);
    let gone = up + std::time::Duration::from_secs(1);
    let show = |app: &mut App| {
        app.howto = None;
        app.howto_quiet = None;
        app.howto_hover = Some(notes);
        app.sync_howto_at(t0);
        app.sync_howto_at(up);
        assert!(app.howto.is_some_and(|t| t.hide.is_none()), "shown");
    };
    // A search for another row: the notes' row is not shown any more,
    // even with the pointer's row still noted.
    show(&mut h.app);
    h.app.section = general;
    h.app.search = "tray".into();
    h.app.filtering = true;
    h.app.howto_hover = Some(notes);
    assert!(h.app.filter_now().is_some_and(|f| !f[notes]), "the search hides it");
    h.app.sync_howto_at(up);
    assert!(h.app.howto.is_some_and(|t| t.hide.is_some()), "it fades out");
    h.app.sync_howto_at(gone);
    assert!(h.app.howto.is_none() && !h.app.howto_moving_at(gone), "then nothing runs");
    // The search box changed: the pointer's row is forgotten.
    h.app.set_search("tray");
    assert_eq!(h.app.howto_hover, None);
    h.app.set_search("");
    h.app.section = general;
    // The window switched away from or minimized (WM_ACTIVATE inactive,
    // WM_SIZE minimized, hidden: set_howto_away): it goes at once and the
    // keyboard's switch does not bring it back until it is in front again.
    show(&mut h.app);
    h.app.keyboard = true;
    h.app.tip_key = true;
    h.app.focus = Some(Hit::Toggle(notes));
    h.app.set_howto_away(true);
    assert!(h.app.howto.is_none() && !h.app.howto_moving_at(up), "away: gone, nothing runs");
    h.app.sync_howto_at(up);
    assert!(h.app.howto.is_none() && !h.app.howto_moving_at(up), "away: the keyboard's switch does not bring it back");
    h.app.set_howto_away(false);
    h.app.sync_howto_at(up);
    assert!(h.app.howto.is_some(), "back in front: the keyboard's card again");
}

/// Reduced motion (Low-spec mode): the keyboard's card is a still frame
/// and no timer runs; switching away takes it off the window at once (a
/// redraw of its own), coming back brings it again (confirm review P2).
#[test]
fn a_still_card_goes_off_the_window_when_it_is_switched_away_from() {
    let mut h = harness(false);
    let _l = crate::i18n::test_lock("en");
    h.app.cfg.effects.low_spec = true;
    assert_eq!(h.app.motion_mode(), crate::motion::Mode::Reduced);
    let general = h.app.sections.iter().position(|s| s.tab == model::Tab::General).unwrap();
    let _ = draw(&mut h.app, general, 820.0);
    SIZE.with(|s| s.set((820.0, 1400.0)));
    h.app.section = general;
    h.app.scroll = 0.0;
    let notes = h.app.rows().iter().position(|r| r.key == "widgets.notes").unwrap();
    h.app.keyboard = true;
    h.app.tip_key = true;
    h.app.focus = Some(Hit::Toggle(notes));
    // Shown (its fade over), still: drawn, nothing moving.
    h.app.redraw();
    let shown = std::time::Instant::now() + std::time::Duration::from_secs(1);
    h.app.howto = h.app.howto.map(|t| super::tooltip::Tip { from: t.from - std::time::Duration::from_secs(1), ..t });
    h.app.redraw();
    assert_eq!(h.app.howto_drawn.get(), Some(notes), "the still card is on the window");
    assert!(!h.app.howto_moving_at(shown), "still: no timer");
    // Switched away: drawn again without it, by itself.
    h.app.set_howto_away(true);
    assert_eq!(h.app.howto_drawn.get(), None, "off the window at once");
    assert!(h.app.howto.is_none());
    // Back: the keyboard's card again, by itself.
    h.app.set_howto_away(false);
    assert!(h.app.howto.is_some_and(|t| t.ri == notes), "the keyboard's card comes back");
}

/// The ring shows only after a key that moves the focus (crate::input):
/// media, volume and letter keys leave it off (a letter typed in the
/// search box too), Tab and the arrows show it, a press of the pointer
/// hides it.
#[test]
fn the_ring_shows_only_after_a_key_that_moves_the_focus() {
    use crate::input::sample::sequence;
    let _l = crate::i18n::test_lock("en");
    let mut h = harness(false);
    h.app.keyboard = false;
    h.app.focus = None;
    sequence("settings", &mut h.app, 0x09, 0x28, |a| a.keyboard, |a, vk| {
        let _ = a.key(vk, false, false, false);
    }, |a| a.pointer_down());
    let src = include_str!("mod.rs");
    assert!(src.contains("a.pointer_down();"), "the window's press goes through pointer_down");
    assert!(include_str!("access.rs").contains("self.keyboard = crate::input::reader_focus(self.keyboard);"));
}

#[test]
fn review_settings_all_buttons_hide_the_ring() {
    use windows::Win32::UI::WindowsAndMessaging::{WM_LBUTTONDOWN, WM_RBUTTONDOWN, WM_MBUTTONDOWN};
    let stage = crate::gfx::promo_kit::stage();
    let cfg = crate::config::Config::default();
    let theme = crate::theme::Theme::graphite();
    let app = unsafe { App::assemble(stage.hwnd, crate::gfx::Gfx::new().unwrap(), client::Client::offline(), cfg.clone(), &cfg,
        crate::syslook::current(), 1.0, &theme, false, true, false).unwrap() };
    super::APP.with(|slot| *slot.borrow_mut() = Some(app));
    let keys = crate::input::sample::Keys::new();
    keys.set(None);
    for msg in [WM_LBUTTONDOWN, WM_RBUTTONDOWN, WM_MBUTTONDOWN] {
        let focus = super::with_app(|a| {
            a.key(0x09, false, false, false);
            assert!(a.keyboard);
            a.focus
        }).unwrap();
        unsafe { super::wndproc(stage.hwnd, msg, WPARAM(0), LPARAM(0x7fff7fff)); }
        super::with_app(|a| {
            assert!(!a.keyboard);
            assert_eq!(a.focus, focus);
        });
    }
    super::APP.with(|slot| slot.borrow_mut().take());
}
