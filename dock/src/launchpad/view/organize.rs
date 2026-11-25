//! Organizing Home on screen (a child of view.rs: it works on the
//! overlay's state): "Tidy Home" from the menu (usage read off the UI
//! thread, Ctrl+Z puts Home back once while Launchpad is open), and in
//! edit mode several icons at once: Ctrl+click, Shift+click, a band over
//! empty space, Ctrl+Space; the row's selected fill behind each picked
//! icon's cell (design system 6.2); their menu (remove from Home, hide,
//! move to a category, group into a
//! folder). Dragging them together is arrange.rs's drag. The pure parts
//! are tidy.rs, picks.rs and usage.rs.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use windows::core::Result;
use windows::UI::Color;
use windows::UI::Composition::{CompositionSpriteShape, Compositor, ShapeVisual};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
use windows_numerics::Vector2;

use super::super::grid::Rect;
use super::super::layout_store;
use super::super::library::{self, Cat};
use super::super::picks::{self, Key};
use super::super::{tidy, usage};
use super::{color, rgba, Focus, MenuAct, Pad};
use crate::config::LpItem;
use crate::i18n::{t, tf};
use crate::search::sources::App;
use crate::theme::{Role, State};

/// The usage read for Tidy Home is in (USAGE).
pub(super) const WM_LP_USAGE: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 5;

/// Usage reads done, by request number (each Launchpad window's requests
/// have their own numbers: NEXT never repeats).
static USAGE: Mutex<Option<Results>> = Mutex::new(None);
static NEXT: AtomicU64 = AtomicU64::new(1);

/// Usage reads done and not taken yet, by request number.
#[derive(Default)]
pub(super) struct Results {
    done: HashMap<u64, HashMap<String, f64>>,
}

impl Results {
    /// Request `n`'s read is done.
    pub fn put(&mut self, n: u64, scores: HashMap<String, f64>) {
        self.done.insert(n, scores);
    }

    /// Request `n`'s message is handled while `newest` is the request the
    /// window waits for: its scores if it is that one (None for an older
    /// one: only the newest is applied). Reads older than the newest go
    /// (an earlier request's, an older window's); the newest stays until
    /// its own message comes.
    pub fn take(&mut self, n: u64, newest: Option<u64>) -> Option<HashMap<String, f64>> {
        let got = self.done.remove(&n);
        let keep_from = newest.unwrap_or(n.saturating_add(1));
        self.done.retain(|k, _| *k >= keep_from);
        got.filter(|_| newest == Some(n))
    }
}

/// A band being dragged over empty space.
pub(in super::super) struct Band {
    start: (f32, f32),
    /// Picked when it started, and whether it adds to them (Ctrl).
    base: Vec<Key>,
    add: bool,
}

/// Edit mode's several-at-once state and Tidy Home's.
pub(in super::super) struct Organize {
    pub picks: picks::Picks,
    pub band: Option<Band>,
    /// Home before the last tidy (Ctrl+Z, once; gone when Launchpad closes).
    pub undo: tidy::Undo,
    /// A usage read asked for (its number).
    pub waiting: Option<u64>,
    /// The picked icons' grounds, and whose are drawn where (in the
    /// page's container): what comes or goes next fades from there.
    picks_v: ShapeVisual,
    drawn: Vec<(Key, Rect)>,
    band_v: ShapeVisual,
    band_shape: CompositionSpriteShape,
    band_geom: windows::UI::Composition::CompositionRectangleGeometry,
}

impl Organize {
    pub fn new(c: &Compositor) -> Result<Self> {
        let picks_v = c.CreateShapeVisual()?;
        picks_v.SetIsVisible(false)?;
        let band_v = c.CreateShapeVisual()?;
        band_v.SetIsVisible(false)?;
        let band_geom = c.CreateRectangleGeometry()?;
        let band_shape = c.CreateSpriteShapeWithGeometry(&band_geom)?;
        band_v.Shapes()?.Append(&band_shape)?;
        Ok(Organize { picks: picks::Picks::default(), band: None, undo: tidy::Undo::default(), waiting: None, picks_v, drawn: Vec::new(), band_v, band_shape, band_geom })
    }

    /// The band over the grid (below the card).
    pub fn insert(&self, ch: &windows::UI::Composition::VisualCollection) -> Result<()> {
        ch.InsertAtTop(&self.band_v)
    }
}

impl Pad {
    // ---- Tidy Home ----------------------------------------------------------------

    /// Can Home be tidied now (the menu's row)? Browsing, not arranging,
    /// the whole app list read and Launchpad's config migrated.
    pub(super) fn can_tidy(&self) -> bool {
        !self.arr.edit && self.can_arrange() && !layout_store::needs_migration(&self.lib.cfg) && self.layout.iter().any(|p| !p.is_empty())
    }

    /// "Tidy Home" was picked: the usage is read on its own thread, then
    /// Home is tidied (usage_arrived).
    pub(super) fn start_tidy(&mut self) {
        if !self.can_tidy() {
            return;
        }
        // (A newer request replaces one still being read: only its result
        // is applied.)
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        self.org.waiting = Some(n);
        let pairs: Vec<(String, String)> = self.apps.iter().map(|a| (a.id.clone(), a.name.clone())).collect();
        let hwnd = self.hwnd.0 as isize;
        let spawned = std::thread::Builder::new().name("launchpad-usage".into()).spawn(move || {
            let s = usage::read(&pairs);
            USAGE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Results::default).put(n, s);
            unsafe {
                let _ = PostMessageW(Some(windows::Win32::Foundation::HWND(hwnd as *mut _)), WM_LP_USAGE, WPARAM(n as usize), LPARAM(0));
            }
        });
        if spawned.is_err() {
            // (No thread: tidied by the order it is in.)
            self.org.waiting = None;
            self.apply_tidy(&HashMap::new());
        }
    }

    /// Usage read `n` is in: Home is tidied with it (if it is the newest
    /// one asked for and Home can still be tidied).
    pub(super) fn usage_arrived(&mut self, n: u64) {
        let got = USAGE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Results::default).take(n, self.org.waiting);
        let Some(scores) = got else { return };
        self.org.waiting = None;
        if self.can_tidy() {
            // (A menu opened meanwhile names places that move: it closes.)
            self.close_menu();
            self.apply_tidy(&scores);
        }
    }

    /// Tidies Home by `scores` (none: the order now for the first page);
    /// the Home before is kept for Ctrl+Z.
    fn apply_tidy(&mut self, scores: &HashMap<String, f64>) {
        let overrides = self.lib.cfg.library.clone();
        let none: HashSet<&str> = HashSet::new();
        let names: HashMap<&str, &str> = self.apps.iter().map(|a| (a.id.as_str(), a.name.as_str())).collect();
        let cat = |id: &str| library::place(id, names.get(id).copied().unwrap_or(""), &overrides, &none);
        let cat_name = |c: Cat| t(c.label_key()).to_string();
        let score = |id: &str| scores.get(id).copied().unwrap_or(0.0);
        let before = self.layout.clone();
        let after = tidy::tidy(&before, self.per_page(), &score, &cat, &cat_name);
        crate::log!("launchpad: Home tidied ({} apps with usage)", scores.len());
        if after != before {
            // Home as saved (its page breaks as arranged, maybe on a
            // bigger screen), or as shown if changed and not saved yet.
            let kept = if self.arr.dirty { layout_store::to_config(&before) } else { self.arr.saved.clone() };
            self.layout = after;
            self.commit_home();
            // (After saving: any change from here on forgets it.)
            let left = self.tidy_mark();
            self.org.undo.remember(kept, left);
            self.go_page(0, true);
        }
        self.announce(t("launchpad.done.tidied").to_string());
    }

    /// Home and the Library as they are now (Home as saved, or as shown
    /// when changed and not saved yet).
    fn tidy_mark(&self) -> tidy::Mark {
        let home = if self.arr.dirty { layout_store::to_config(&self.layout) } else { self.arr.saved.clone() };
        tidy::Mark { home, hidden: self.lib.cfg.hidden.clone(), library: self.lib.cfg.library.clone() }
    }

    /// Something on Home was changed (edit mode): to be saved, and Tidy
    /// Home's undo is gone (it would write over the change).
    pub(super) fn edited(&mut self) {
        self.arr.dirty = true;
        self.org.undo.forget();
    }

    /// Ctrl+Z: Home as it was before the last tidy (once), only while
    /// nothing else was changed since.
    pub(super) fn undo_tidy(&mut self) {
        if self.arr.dragging() || self.arr.folder.is_some() || !self.org.undo.armed() {
            return;
        }
        let now = self.tidy_mark();
        let Some(prev) = self.org.undo.take(&now) else { return };
        crate::log!("launchpad: Home put back as it was before tidying");
        if self.arr.edit {
            self.end_edit();
        }
        self.layout = prev;
        self.commit_home();
        self.announce(t("launchpad.done.untidied").to_string());
    }

    // ---- several at once (edit mode) ---------------------------------------------

    /// Who the item at listing index `i` is.
    fn key_at(&self, i: usize) -> Option<Key> {
        self.item_at(i).map(picks::key_of)
    }

    /// The name of the item at listing index `i`.
    fn name_at(&self, i: usize) -> String {
        self.item_at(i).and_then(|it| self.item_tile(it)).map(|t| t.name).unwrap_or_default()
    }

    /// The apps picked (a folder's apps where it is), in the pages' order.
    fn picked_ids(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for p in self.org.picks.positions(&self.layout) {
            match self.layout.get(p.page).and_then(|v| v.get(p.slot)) {
                Some(LpItem::App(id)) => out.push(id.clone()),
                Some(LpItem::Folder { apps, .. }) => out.extend(apps.iter().cloned()),
                None => {}
            }
        }
        let mut once = HashSet::new();
        out.retain(|id| once.insert(id.clone()));
        out
    }

    /// Ctrl+click (`ctrl` alone), Shift+click (`shift`; with Ctrl it adds)
    /// on the item at listing index `i`.
    pub(super) fn pick_click(&mut self, i: usize, ctrl: bool, shift: bool) {
        let Some(k) = self.key_at(i) else { return };
        self.sel = Some(i);
        self.focus = Focus::Grid;
        if shift {
            self.org.picks.range(&self.layout, k, ctrl);
            self.say_picked_n();
        } else {
            self.toggle_pick_at(i);
            return;
        }
        self.update_sel();
        self.publish();
    }

    /// The item at listing index `i` picked or not (Ctrl+click,
    /// Ctrl+Space, a screen reader's Toggle).
    pub(super) fn toggle_pick_at(&mut self, i: usize) {
        let Some(k) = self.key_at(i) else { return };
        let on = self.org.picks.toggle(k);
        let name = self.name_at(i);
        self.update_sel();
        self.announce(tf(if on { "launchpad.edit.picked" } else { "launchpad.edit.unpicked" }, &[("name", &name)]));
    }

    /// A plain click on the item at listing index `i`: nothing stays
    /// picked, and a Shift+click runs from it.
    pub(super) fn plain_click(&mut self, i: usize) {
        self.clear_picks();
        if let Some(k) = self.key_at(i) {
            self.org.picks.set_anchor(k);
        }
    }

    /// Nothing picked any more (a plain click, Esc, edit mode ending).
    pub(super) fn clear_picks(&mut self) {
        self.org.band = None;
        let _ = self.org.band_v.SetIsVisible(false);
        if !self.org.picks.is_empty() {
            self.org.picks.clear();
            self.draw_picks();
        }
    }

    /// "{count} selected", said.
    pub(super) fn say_picked_n(&mut self) {
        let n = self.org.picks.len();
        self.announce(tf("launchpad.edit.picked_n", &[("count", &n.to_string())]));
    }

    /// Is the item at listing index `i` picked?
    pub(super) fn is_picked(&self, i: usize) -> bool {
        self.key_at(i).is_some_and(|k| self.org.picks.contains(&k))
    }

    /// The picked icons' grounds on the page shown, as the keyboard or the
    /// pointer (whichever moved last) picked them.
    pub(super) fn draw_picks(&mut self) {
        self.draw_picks_by(self.by_key);
    }

    /// The picked icons' grounds on the page shown (6.2: the row's selected
    /// fill behind each one's cell, the pointer's ground's shape; in the
    /// page's container, so they scale and slide with it). One the pointer
    /// picked comes over 90 ms, one it let go leaves over 120; `instant`
    /// (the keyboard did it, or the page turned): at once (6.3). High
    /// contrast: Highlight behind the icon alone (its name keeps its own
    /// ground and color), at once.
    fn draw_picks_by(&mut self, instant: bool) {
        let v = self.org.picks_v.clone();
        let shown = self.arr.edit && !self.searching && !self.on_lib() && !self.org.picks.is_empty();
        let container = self.pages.get(&self.page).map(|p| p.container.clone()).filter(|_| shown);
        let Some(container) = container else {
            let _ = v.SetIsVisible(false);
            self.org.drawn.clear();
            return;
        };
        let Ok(shapes) = v.Shapes() else { return };
        let _ = shapes.Clear();
        let same = v.Parent().ok().is_some_and(|p| p == container);
        if !same {
            if let Ok(p) = v.Parent() {
                let _ = p.Children().and_then(|ch| ch.Remove(&v));
            }
            // (Under the icons and their names.)
            if container.Children().and_then(|ch| ch.InsertAtBottom(&v)).is_err() {
                return;
            }
        }
        let _ = v.SetSize(Vector2 { X: self.w, Y: self.h });
        let hc = self.hc().is_some();
        let fill = color(rgba(self.theme.fill_of(Role::Row, State::Selected)));
        let clear = Color { A: 0, ..fill };
        let (pp, gap) = (self.per_page(), self.arr.gap_on(self.page));
        let mut now: Vec<(Key, Rect)> = Vec::new();
        for slot in 0..pp {
            let Some(k) = super::super::edit::item_in_slot(slot, gap) else { continue };
            let Some(it) = self.layout.get(self.page).and_then(|p| p.get(k)) else { continue };
            let key = picks::key_of(it);
            if !self.org.picks.contains(&key) {
                continue;
            }
            let r = if hc {
                let (i, out) = (self.icon_rect(&self.grid, slot), crate::gfx::parts::icon_ring().0);
                Rect::new(i.l - out, i.t - out, i.r + out, i.b + out)
            } else {
                self.grid.item(slot, self.rtl).offset(self.work.l, self.work.t)
            };
            now.push((key, r));
        }
        // An icon's shape behind an icon, a card's corners behind a cell.
        let radius = |r: &Rect| if hc { r.w() * crate::theme::SQUIRCLE_ARC } else { self.theme.radii().card };
        let was = std::mem::take(&mut self.org.drawn);
        let fades = !instant && same && !hc;
        let speed = crate::effects::current().speed;
        let (on, off) = (super::motion::pointer(self.animate(), true, hc, speed), super::motion::pointer(self.animate(), false, hc, speed));
        let c = self.gfx.compositor.clone();
        let add = |r: &Rect, from: Option<Color>, to: Color, plan: (i64, crate::motion::Curve)| -> Result<()> {
            let g = c.CreateRoundedRectangleGeometry()?;
            g.SetSize(Vector2 { X: r.w(), Y: r.h() })?;
            let rad = radius(r);
            g.SetCornerRadius(Vector2 { X: rad, Y: rad })?;
            let s = c.CreateSpriteShapeWithGeometry(&g)?;
            let brush = c.CreateColorBrushWithColor(from.unwrap_or(to))?;
            if from.is_some() {
                super::arrange::color_to(&c, &brush, from, to, plan);
            }
            s.SetFillBrush(&brush)?;
            s.SetOffset(Vector2 { X: r.l, Y: r.t })?;
            shapes.Append(&s)
        };
        for (k, r) in &now {
            let fresh = fades && !was.iter().any(|(w, _)| w == k);
            let _ = add(r, fresh.then_some(clear), fill, on);
        }
        // What was let go fades out where it was.
        for (k, r) in was.iter().filter(|_| fades) {
            if !now.iter().any(|(n, _)| n == k) {
                let _ = add(r, Some(fill), clear, off);
            }
        }
        self.org.drawn = now;
        let _ = v.SetIsVisible(true);
    }

    // ---- the band -------------------------------------------------------------------

    /// A press on empty space moved: a band from where it started.
    pub(super) fn band_start(&mut self, x: f32, y: f32) {
        let add = super::key_down(windows::Win32::UI::Input::KeyboardAndMouse::VK_CONTROL);
        self.org.band = Some(Band { start: (x, y), base: self.org.picks.keys().to_vec(), add });
        let (fill, line) = match self.hc() {
            Some(hc) => (super::with_alpha(rgba(hc.highlight), 0.0), rgba(hc.highlight)),
            None => (rgba(self.theme.fill_row(State::Selected)), rgba(self.theme.hairline())),
        };
        let c = self.gfx.compositor.clone();
        if let (Ok(f), Ok(l)) = (c.CreateColorBrushWithColor(color(fill)), c.CreateColorBrushWithColor(color(line))) {
            let _ = self.org.band_shape.SetFillBrush(&f);
            let _ = self.org.band_shape.SetStrokeBrush(&l);
            let _ = self.org.band_shape.SetStrokeThickness(1.0);
        }
        let _ = self.org.band_v.SetSize(Vector2 { X: self.w, Y: self.h });
    }

    /// The band follows the pointer; what it touches on the page shown is
    /// picked at once.
    pub(super) fn band_move(&mut self, x: f32, y: f32) {
        let Some(b) = &self.org.band else { return };
        let r = band_rect(b.start, (x, y));
        let _ = self.org.band_geom.SetOffset(Vector2 { X: r.l, Y: r.t });
        let _ = self.org.band_geom.SetSize(Vector2 { X: r.w().max(1.0), Y: r.h().max(1.0) });
        let _ = self.org.band_v.SetIsVisible(true);
        let mut boxes = Vec::new();
        for slot in 0..self.per_page() {
            let Some(i) = self.index_of_slot(slot) else { continue };
            let Some(k) = self.key_at(i) else { continue };
            let g = self.grid.icon(slot, self.rtl).offset(self.work.l, self.work.t);
            let ((l, t_), (rr, bb)) = (self.from_grid(g.l, g.t), self.from_grid(g.r, g.b));
            boxes.push((Rect::new(l, t_, rr, bb), k));
        }
        let hit = picks::in_band(&r, &boxes);
        let (base, add) = (b.base.clone(), b.add);
        let before = self.org.picks.clone();
        self.org.picks.set(&base, hit, add);
        if self.org.picks != before {
            // (The pointer picks: they fade.)
            self.draw_picks_by(false);
        }
    }

    /// The band is let go: what it touched stays picked.
    pub(super) fn band_end(&mut self) {
        if self.org.band.take().is_none() {
            return;
        }
        let _ = self.org.band_v.SetIsVisible(false);
        self.focus = Focus::Grid;
        self.say_picked_n();
    }

    /// A band ends without a release (the capture lost): as it is.
    pub(super) fn band_cancel(&mut self) {
        if self.org.band.take().is_some() {
            let _ = self.org.band_v.SetIsVisible(false);
        }
    }

    // ---- their menu -------------------------------------------------------------

    /// Edit mode's menu on the item at listing index `i`: for the picked
    /// ones (it picked alone first if it was not).
    pub(super) fn open_picks_menu(&mut self, i: usize, at: (f32, f32), keyboard: bool) {
        let Some(k) = self.key_at(i) else { return };
        if !self.org.picks.contains(&k) {
            self.org.picks.clear();
            self.org.picks.toggle(k);
        }
        self.sel = Some(i);
        self.focus = Focus::Grid;
        self.update_sel();
        let rows = self.picks_rows();
        if rows.is_empty() {
            return;
        }
        let n = self.org.picks.len();
        let name = if n == 1 { self.name_at(i) } else { tf("launchpad.edit.picked_n", &[("count", &n.to_string())]) };
        let _ = keyboard;
        self.announce(name.clone());
        self.ask_menu(rows, App { name, id: String::new() }, at);
    }

    /// The rows of the picked items' menu: Group into Folder (two or
    /// more), then what an app's menu does to Home and the Library.
    fn picks_rows(&self) -> Vec<Option<(String, MenuAct)>> {
        let lib_ok = self.apps_complete && !layout_store::needs_migration(&self.lib.cfg) && !self.picked_ids().is_empty();
        picks_menu(self.org.picks.len(), lib_ok, self.lib.cfg.show_library)
    }

    /// "Move to category" for the picked apps (its submenu's rows): their
    /// category checked when they share one.
    pub(super) fn picks_category_rows(&self) -> Vec<(String, MenuAct, bool)> {
        let none: HashSet<&str> = HashSet::new();
        let ids = self.picked_ids();
        let cats: HashSet<Cat> = ids.iter().map(|id| library::place(id, self.app_by_id(id).map_or("", |a| a.name.as_str()), &self.lib.cfg.library, &none)).collect();
        let now = (cats.len() == 1).then(|| cats.into_iter().next()).flatten();
        Cat::PICKABLE.into_iter().map(|c| (t(c.label_key()).to_string(), MenuAct::ManySetCat(c), Some(c) == now)).collect()
    }

    /// A row of the picked items' menu.
    pub(super) fn picks_act(&mut self, act: &MenuAct) {
        let ids = self.picked_ids();
        let count = ids.len().to_string();
        match act {
            MenuAct::Group => {
                let ps = self.org.picks.positions(&self.layout);
                let pp = self.per_page();
                let Some((at, new)) = picks::group(&mut self.layout, &ps, t("launchpad.folder.default"), pp) else { return };
                self.edited();
                self.org.picks.clear();
                let i = self.flat(at);
                self.sel = Some(i);
                self.refresh_after_change(at.page);
                self.open_folder(i);
                if new {
                    self.start_rename(false);
                }
                let n = match self.item_at(i) {
                    Some(LpItem::Folder { apps, .. }) => apps.len(),
                    _ => 0,
                };
                self.announce(tf("launchpad.done.grouped", &[("count", &n.to_string()), ("name", &self.folder_name())]));
            }
            MenuAct::ManyRemoveHome | MenuAct::ManyHide | MenuAct::ManySetCat(_) => {
                let mut changed = false;
                for id in &ids {
                    changed |= match act {
                        MenuAct::ManyRemoveHome => layout_store::remove_from_home(&mut self.layout, id),
                        MenuAct::ManyHide => layout_store::hide(&mut self.layout, &mut self.lib.cfg.hidden, id),
                        MenuAct::ManySetCat(c) => {
                            let name = self.app_by_id(id).map(|a| a.name.clone()).unwrap_or_default();
                            library::set_category(&mut self.lib.cfg.library, id, &name, *c)
                        }
                        _ => false,
                    };
                }
                let said = match act {
                    MenuAct::ManyRemoveHome => tf("launchpad.done.removed_n", &[("count", &count)]),
                    MenuAct::ManyHide => tf("launchpad.done.hidden_n", &[("count", &count)]),
                    MenuAct::ManySetCat(c) => tf("launchpad.done.moved_n", &[("count", &count), ("category", t(c.label_key()))]),
                    _ => String::new(),
                };
                if !matches!(act, MenuAct::ManySetCat(_)) {
                    self.org.picks.clear();
                }
                if changed {
                    self.commit_home();
                }
                self.draw_picks();
                self.announce(said);
            }
            _ => {}
        }
    }

    /// The pages changed in edit mode: bound again in place on page `page`.
    fn refresh_after_change(&mut self, page: usize) {
        let n = self.pages_n();
        let gone: Vec<usize> = self.pages.keys().copied().filter(|&p| p >= n).collect();
        for p in gone {
            if let Some(v) = self.pages.remove(&p) {
                if self.sel_in == Some(Some(p)) {
                    self.detach_sel();
                }
                let _ = self.strip.Children().and_then(|ch| ch.Remove(&v.container));
            }
        }
        let shown: Vec<usize> = self.pages.keys().copied().collect();
        for p in shown {
            self.bind_page(p);
        }
        self.go_page(page.min(n.saturating_sub(1)), true);
        self.update_sel();
    }
}

/// Pure: the picked items' menu with `n` picked: Group into Folder (two
/// or more); then, when Home and the Library can change (`lib_ok`: the
/// whole app list read, apps among them), Remove from Home and Move to
/// Category… (with the Library shown) and Hide, as an app's menu has them.
fn picks_menu(n: usize, lib_ok: bool, library: bool) -> Vec<Option<(String, MenuAct)>> {
    let mut rows: Vec<Option<(String, MenuAct)>> = Vec::new();
    if n >= 2 {
        rows.push(Some((t("launchpad.menu.group").to_string(), MenuAct::Group)));
    }
    if lib_ok && n > 0 {
        if !rows.is_empty() {
            rows.push(None);
        }
        if library {
            rows.push(Some((t("launchpad.menu.remove_home").to_string(), MenuAct::ManyRemoveHome)));
            rows.push(Some((t("launchpad.menu.move_to").to_string(), MenuAct::ManyMoveTo)));
        }
        rows.push(Some((t("launchpad.menu.hide").to_string(), MenuAct::ManyHide)));
    }
    rows
}

/// The mouse message being handled came from a finger or a pen (Windows
/// marks those it makes into mouse messages).
pub(super) fn from_touch() -> bool {
    let info = unsafe { windows::Win32::UI::WindowsAndMessaging::GetMessageExtraInfo() }.0 as usize as u32;
    touch_signature(info)
}

/// Pure: does a message's extra info carry the pen / touch signature?
fn touch_signature(info: u32) -> bool {
    info & 0xFFFF_FF00 == 0xFF51_5700
}

/// Pure: where a band's rectangle is (its two corners in any order).
fn band_rect(a: (f32, f32), b: (f32, f32)) -> Rect {
    Rect::new(a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_band_is_its_corners_either_way() {
        assert_eq!(band_rect((10.0, 20.0), (4.0, 30.0)), Rect::new(4.0, 20.0, 10.0, 30.0));
        assert_eq!(band_rect((1.0, 1.0), (1.0, 1.0)).w(), 0.0);
    }

    fn acts(rows: &[Option<(String, MenuAct)>]) -> Vec<Option<MenuAct>> {
        rows.iter().map(|r| r.as_ref().map(|(_, a)| a.clone())).collect()
    }

    #[test]
    fn the_picked_items_menu_by_how_many() {
        let _l = crate::i18n::test_lock("ko");
        use MenuAct::*;
        assert_eq!(acts(&picks_menu(1, true, true)), [Some(ManyRemoveHome), Some(ManyMoveTo), Some(ManyHide)], "one: as an app's menu, no grouping");
        assert_eq!(acts(&picks_menu(3, true, true)), [Some(Group), None, Some(ManyRemoveHome), Some(ManyMoveTo), Some(ManyHide)], "two or more: grouping first");
        assert_eq!(acts(&picks_menu(3, true, false)), [Some(Group), None, Some(ManyHide)], "no Library: hide only");
        assert_eq!(acts(&picks_menu(2, false, true)), [Some(Group)], "the app list incomplete: Home and the Library stay");
        assert!(picks_menu(1, false, true).is_empty() && picks_menu(0, true, true).is_empty());
        let rows = picks_menu(2, true, true);
        assert_eq!(rows[0].as_ref().unwrap().0, "폴더로 묶기");
        assert_eq!(rows[2].as_ref().unwrap().0, "고정 해제", "the same words as an app's menu");
    }

    #[test]
    fn tidy_home_sits_next_to_edit_home_screen() {
        let _l = crate::i18n::test_lock("ko");
        assert_eq!(acts(&super::super::menu_arrange_rows(true)), [Some(MenuAct::Arrange), Some(MenuAct::Tidy)]);
        assert_eq!(acts(&super::super::menu_arrange_rows(false)), [Some(MenuAct::Arrange)], "only when Home can be tidied");
        let rows = super::super::menu_arrange_rows(true);
        assert_eq!((rows[0].as_ref().unwrap().0.as_str(), rows[1].as_ref().unwrap().0.as_str()), ("고정 항목 편집", "고정 항목 정렬"));
    }

    fn s(v: f64) -> HashMap<String, f64> {
        [("a".to_string(), v)].into()
    }

    /// The review's case: two tidies asked for, their reads done and their
    /// messages handled in every order: the newest is applied exactly
    /// once, never an older one, and nothing is lost to a shared slot.
    #[test]
    fn only_the_newest_usage_read_is_applied_once() {
        let (a, b) = (1, 2);
        // (done first, done second, message handled first, second)
        for (d1, d2, m1, m2) in [(a, b, a, b), (a, b, b, a), (b, a, a, b), (b, a, b, a)] {
            let mut r = Results::default();
            let mut waiting = Some(b);
            let mut applied = Vec::new();
            r.put(d1, s(d1 as f64));
            r.put(d2, s(d2 as f64));
            for m in [m1, m2] {
                if let Some(got) = r.take(m, waiting) {
                    waiting = None;
                    applied.push(got["a"]);
                }
            }
            assert_eq!(applied, [b as f64], "done {d1},{d2}; handled {m1},{m2}");
            assert!(r.done.is_empty(), "nothing left behind");
        }
        // The older message handled before the newest read is done.
        let mut r = Results::default();
        r.put(a, s(1.0));
        assert_eq!(r.take(a, Some(b)), None);
        r.put(b, s(2.0));
        assert_eq!(r.take(b, Some(b)).map(|x| x["a"]), Some(2.0));
        // An older window's read never reaches a newer window's request,
        // and goes when the newer one's comes.
        let mut r = Results::default();
        r.put(7, s(7.0));
        r.put(9, s(9.0));
        assert_eq!(r.take(9, Some(9)).map(|x| x["a"]), Some(9.0));
        assert!(r.done.is_empty());
        assert_eq!(r.take(7, None), None);
    }

    #[test]
    fn touch_and_pen_are_told_from_the_mouse() {
        assert!(touch_signature(0xFF51_5780), "touch");
        assert!(touch_signature(0xFF51_5700), "pen");
        assert!(!touch_signature(0), "a mouse");
        assert!(!touch_signature(0x1234_5678));
    }

    #[test]
    fn timers_and_messages_stay_distinct() {
        assert!(![super::super::WM_LP_APPS, super::super::WM_LP_MORE, super::super::WM_LP_CLOSED, super::super::super::loader::WM_LP_ICONS].contains(&WM_LP_USAGE));
    }
}
