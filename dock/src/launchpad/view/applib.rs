//! The App Library on screen (a child of view.rs: it works on the
//! overlay's state): the Library page after the Home pages, its category
//! tiles (big icons open their app, the rest opens the category in the
//! folder's panel, arrange.rs), the Home / Library actions of the apps'
//! menus (add to / remove from Home, move to a category, hide, unhide),
//! and the "new" dot. (Uninstallers, web links and documents start in the
//! Library, not on a Home NopBar makes: layout_store::migrate.)
//!
//! The pure part is in library.rs, cleanup.rs and layout_store.rs. Every
//! colour and corner comes from the theme's tokens (`look_of`: a tile is a
//! raised thing at rest, 2.4; what is not loaded yet shows in the icons'
//! own shape); each new surface is drawn by one function:
//! paint_category_tile, paint_new_dot, paint_library_dot. Nothing here animates
//! by itself: a category opens with the folder's motion (arrange.rs), page
//! turns with the strip's (view.rs), so motion::allowed and the effects
//! settings decide, as everywhere in Launchpad.

use std::collections::{HashMap, HashSet};
use std::mem::ManuallyDrop;

use windows::core::Result;
use windows::UI::Composition::{CompositionStretch, CompositionSurfaceBrush, ContainerVisual, SpriteVisual};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{
    ID2D1DeviceContext, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1, D2D1_ELLIPSE, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
};
use windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_ALIGNMENT_LEADING;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_APPS, VK_DOWN, VK_F10, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_UP};
use windows_numerics::{Matrix3x2, Vector2, Vector3};

use super::super::cleanup;
use super::super::grid::{Arrow, Rect};
use super::super::layout_store;
use super::super::library::{self, Cat, Group, Part, Tiles};
use super::super::loader;
use super::{draw_layout, key_down, rgba, stroke_rr, After, Deferred, Focus, Hit, MenuAct, Pad};
use crate::config::LaunchpadCfg;
use crate::i18n::{t, tf};
use crate::search::sources::App;
use crate::theme::metrics::{size, space};
use crate::theme::{Rgba, Role, State, Theme};

// ---- sizes (DIP; the colours and corners are the theme's: `Look`) ------------

/// The colours and corners new surfaces draw with, all from the theme's
/// tokens (high contrast: the system's colours). Achromatic: no accent
/// colour anywhere.
struct Look {
    /// A tile's ground: the raised fill at rest (2.4).
    ground: Rgba,
    /// Where an icon not loaded yet will be (arrange::slot_fill).
    slot: Rgba,
    /// High contrast's outline instead of the ground (color, DIP).
    line: Option<(Rgba, f32)>,
    /// The "new" dot: the text colour.
    mark: D2D1_COLOR_F,
    radii: crate::theme::Radii,
    /// The theme (a tile is raised: Flat v2's card light round it).
    theme: Theme,
}

/// Pure: the Library's look in `theme`.
fn look_of(theme: &Theme) -> Look {
    Look {
        ground: theme.fill_of(Role::Raised, State::Rest),
        slot: super::arrange::slot_fill(theme),
        line: theme.outline_of(Role::Raised, State::Rest),
        mark: rgba(theme.text_primary()),
        radii: theme.radii(),
        theme: theme.clone(),
    }
}

/// The Library page's visuals: per tile its picture and its name.
pub(super) struct LibCells {
    container: ContainerVisual,
    tiles: Vec<(SpriteVisual, SpriteVisual)>,
}

pub(super) struct Lib {
    /// Launchpad's section as saved (its `layout` is not read: Home is
    /// arrange.rs's `saved`).
    pub cfg: LaunchpadCfg,
    pub groups: Vec<Group>,
    /// Every app that looks like clutter, and why.
    pub reasons: HashMap<String, cleanup::Reason>,
    /// New apps (a dot until opened or moved).
    pub new: HashSet<String>,
    hidden: HashSet<String>,
    pub tiles: Tiles,
    pub view: Option<LibCells>,
    /// The tile with the keyboard's place.
    pub sel: Option<usize>,
    brushes: HashMap<String, CompositionSurfaceBrush>,
}

impl Lib {
    pub fn new() -> Self {
        Lib {
            cfg: LaunchpadCfg::default(),
            groups: Vec::new(),
            reasons: HashMap::new(),
            new: HashSet::new(),
            hidden: HashSet::new(),
            tiles: Tiles::none(),
            view: None,
            sel: None,
            brushes: HashMap::new(),
        }
    }

    pub fn is_hidden(&self, id: &str) -> bool {
        self.hidden.contains(id)
    }
}

// ---- drawing: one function per component ------------------------------------------

/// The "new" dot (centre `cx`, `cy`).
pub(super) unsafe fn paint_new_dot(dc: &ID2D1DeviceContext, cx: f32, cy: f32, c: D2D1_COLOR_F) -> Result<()> {
    let r = library::NEW_DOT / 2.0;
    let b = dc.CreateSolidColorBrush(&c, None)?;
    dc.FillEllipse(&D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: r, radiusY: r }, &b);
    Ok(())
}

/// The Library's page dot `size` DIP across (grid::DotsRow): four little
/// squares (filled, or outlined in high contrast when it is not the page
/// shown).
pub(super) unsafe fn paint_library_dot(dc: &ID2D1DeviceContext, cx: f32, cy: f32, size: f32, c: D2D1_COLOR_F, filled: bool) -> Result<()> {
    let e = size * 0.44;
    let gap = size * 0.14;
    let b = dc.CreateSolidColorBrush(&c, None)?;
    for k in 0..4 {
        let (row, col) = ((k / 2) as f32, (k % 2) as f32);
        let l = cx - e - gap / 2.0 + col * (e + gap);
        let t = cy - e - gap / 2.0 + row * (e + gap);
        let r = D2D_RECT_F { left: l, top: t, right: l + e, bottom: t + e };
        if filled {
            dc.FillRectangle(&r, &b);
        } else {
            dc.DrawRectangle(&r, &b, 1.0, None);
        }
    }
    Ok(())
}

/// An icon's pixels fitted in `r` (aspect kept), drawn with the cubic filter.
pub(super) unsafe fn paint_icon(dc: &ID2D1DeviceContext, i: &crate::icons::Icon, r: Rect) -> Result<()> {
    if crate::gfx::bgra_len(i.width, i.height) != Some(i.pixels.len()) {
        return Ok(());
    }
    let props = D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: 96.0,
        dpiY: 96.0,
        bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
        colorContext: ManuallyDrop::new(None),
    };
    let bmp = dc.CreateBitmap(D2D_SIZE_U { width: i.width, height: i.height }, Some(i.pixels.as_ptr() as _), i.width * 4, &props)?;
    let (dw, dh) = crate::icons::fit_box(i.width, i.height, 1000);
    let k = r.w().min(r.h()) / 1000.0;
    let (w, h) = (dw * k, dh * k);
    let d = D2D_RECT_F { left: r.cx() - w / 2.0, top: r.cy() - h / 2.0, right: r.cx() + w / 2.0, bottom: r.cy() + h / 2.0 };
    dc.DrawBitmap(&bmp, Some(&d), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
    Ok(())
}

/// Pure: a Library category tile's corners: the theme's tile radius, a
/// quarter of a tile shrunk past it at most.
fn library_tile_radius(tile: f32, side: f32) -> f32 {
    tile.min(side / 4.0)
}

/// What a category tile shows: its big apps' and mini apps' icons (None:
/// not loaded yet, a placeholder) and which big ones are new.
struct TileArt {
    big: Vec<(Option<std::sync::Arc<crate::icons::Icon>>, bool)>,
    mini: Vec<Option<std::sync::Arc<crate::icons::Icon>>>,
}

/// A category tile `side` DIP square: a tile (r.tile; the raised fill at
/// rest, in high contrast its outline), its first apps' icons big in three
/// (or four) quadrants, the next ones small in the last; a new app's big
/// icon carries the dot. An icon not loaded yet: its place in the icons'
/// own shape.
unsafe fn paint_category_tile(dc: &ID2D1DeviceContext, side: f32, art: &TileArt, look: &Look, rtl: bool) -> Result<()> {
    dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
    // A tile's corners, shrunk with a tile for a small work area.
    let radius = library_tile_radius(look.radii.tile, side);
    let rect = D2D_RECT_F { left: 0.0, top: 0.0, right: side, bottom: side };
    match look.line {
        Some((c, lw)) => stroke_rr(dc, rgba(c), &super::arrange::inner(rect, radius, lw), lw)?,
        // (Raised: Flat v2's card light round it, inside its picture.)
        None => crate::gfx::parts::card(dc, crate::gfx::parts::card_room(&look.theme, State::Rest, rect, 1.0), radius, &look.theme, State::Rest, look.ground, 1.0, 1.0)?,
    }
    let slot = |dc: &ID2D1DeviceContext, icon: &Option<std::sync::Arc<crate::icons::Icon>>, r: Rect| -> Result<()> {
        match icon {
            Some(i) => paint_icon(dc, i, r),
            None => super::arrange::squircle(dc, r.l, r.t, r.w().min(r.h()), look.slot, look.line),
        }
    };
    for (q, (icon, new)) in art.big.iter().enumerate() {
        let r = library::big_icon(q, side, rtl);
        slot(dc, icon, r)?;
        if *new {
            let d = library::NEW_DOT / 2.0;
            let cx = if rtl { r.l + d } else { r.r - d };
            paint_new_dot(dc, cx, r.t + d, look.mark)?;
        }
    }
    for (k, icon) in art.mini.iter().enumerate() {
        slot(dc, icon, library::mini_icon(k, side, rtl))?;
    }
    Ok(())
}

/// A category's glyph (the design kit's Library tiles).
fn cat_glyph(c: Cat) -> crate::glyphs::Glyph {
    use crate::glyphs::Glyph;
    match c {
        Cat::Dev => Glyph::Terminal,
        Cat::Games => Glyph::Gamepad,
        Cat::Creative => Glyph::Image,
        Cat::Office => Glyph::Document,
        Cat::Internet => Glyph::Globe,
        Cat::Utilities => Glyph::Gear,
        Cat::Other => Glyph::Desktops,
        Cat::Hidden => Glyph::EyeOff,
    }
}

/// Pure: how wide a one-line layout is made once measured: as wide as
/// its text (a pixel of room for rounding), never wider than it may be.
/// Drawn text, the place it is put at and what a click hits are then the
/// same box, right to left too (a wider box would right-align the text
/// inside it and push it past where it was placed).
fn tight_width(text_w: f32, max_w: f32) -> f32 {
    (text_w.ceil() + 1.0).min(max_w.max(1.0))
}

impl Pad {
    /// One line of text laid out as wide as it is (tight_width), at most
    /// `max_w`: (layout, its width, height).
    fn tight_layout(&self, text: &str, size: f32, bold: bool, max_w: f32) -> Option<(windows::Win32::Graphics::DirectWrite::IDWriteTextLayout, f32, f32)> {
        self.layout(text, size, bold, max_w, 1, DWRITE_TEXT_ALIGNMENT_LEADING).map(|(l, tw, th)| {
            let fw = tight_width(tw, max_w);
            unsafe {
                let _ = l.SetMaxWidth(fw);
            }
            (l, fw, th)
        })
    }

    // ---- the look --------------------------------------------------------------

    /// The theme's tokens for the Library's surfaces.
    fn look(&self) -> Look {
        look_of(&self.theme)
    }

    /// The "new" dot's colour: the text colour (achromatic).
    pub(super) fn new_dot_color(&self) -> D2D1_COLOR_F {
        self.look().mark
    }

    // ---- state -----------------------------------------------------------------

    /// The Library page's index (None: it does not show: off in Settings,
    /// no apps, or an icon being dragged on the Home pages).
    pub(super) fn lib_page(&self) -> Option<usize> {
        let shown = self.lib.cfg.show_library && !self.apps.is_empty() && !self.arr.dragging();
        library::page(self.layout.len().max(1), shown)
    }

    /// The Library page is on screen (not searching).
    pub(super) fn on_lib(&self) -> bool {
        !self.searching && self.lib_page() == Some(self.page)
    }

    /// The Library again from the apps, Home and the user's choices.
    pub(super) fn lib_recompute(&mut self) {
        let pairs: Vec<(String, String)> = self.apps.iter().map(|a| (a.id.clone(), a.name.clone())).collect();
        self.lib.reasons = cleanup::reasons(&pairs);
        self.lib.hidden = self.lib.cfg.hidden.iter().cloned().collect();
        let home_ids = layout_store::home_ids(&self.layout);
        let home: HashSet<&str> = home_ids.iter().map(|s| s.as_str()).collect();
        let hidden: HashSet<&str> = self.lib.hidden.iter().map(|s| s.as_str()).collect();
        let reasons = &self.lib.reasons;
        let groups = library::groups(&pairs, &self.lib.cfg.library, &hidden, &home, &|id| reasons.contains_key(id));
        // New: only once the config is migrated and the whole list is read
        // (else every app would look new).
        let new: HashSet<String> = if layout_store::needs_migration(&self.lib.cfg) || !self.apps_complete {
            HashSet::new()
        } else {
            let ids: Vec<String> = pairs.iter().map(|(i, _)| i.clone()).collect();
            layout_store::new_apps(&ids, &self.lib.cfg.seen).into_iter().filter(|id| !hidden.contains(id.as_str()) && !home.contains(id.as_str())).collect()
        };
        self.lib.new = new;
        // (In the grid's region, on whole device pixels: drawn where a
        // click finds them.)
        self.lib.tiles = library::tiles(groups.len(), &self.grid, &self.regions().grid).on_pixels(self.s, (self.work.l, self.work.t));
        self.lib.sel = self.lib.sel.filter(|&k| k < groups.len());
        self.lib.groups = groups;
    }

    /// Launchpad's section as it is now (Home: as saved).
    fn lp_state(&self) -> LaunchpadCfg {
        LaunchpadCfg { layout: self.arr.saved.clone(), ..self.lib.cfg.clone() }
    }

    /// Saves Launchpad's section (between messages: view::run_deferred).
    pub(super) fn lp_save(&mut self) {
        let s = self.lp_state();
        self.deferred.push(Deferred::SaveLp(s));
    }

    /// App `id` was opened or moved: not new any more (saved).
    pub(super) fn lib_seen(&mut self, id: &str) {
        if self.lib.new.remove(id) && layout_store::mark_seen(&mut self.lib.cfg.seen, id) {
            self.lp_save();
        }
    }

    /// An app as said aloud: its name, and whether it is hidden or new.
    pub(super) fn app_said(&self, a: &App) -> String {
        if self.lib.is_hidden(&a.id) {
            tf("launchpad.library.hidden_app", &[("name", &a.name)])
        } else if self.lib.new.contains(&a.id) {
            tf("launchpad.library.new", &[("name", &a.name)])
        } else {
            a.name.clone()
        }
    }

    /// Tile `k` as said aloud: "Games, category, 12 apps".
    pub(super) fn tile_said(&self, k: usize) -> String {
        let Some(g) = self.lib.groups.get(k) else { return String::new() };
        tf("launchpad.library.a11y", &[("name", t(g.cat.label_key())), ("count", &g.apps.len().to_string())])
    }

    /// Home or the Library changed: the pages again, on the page the user
    /// is on (the Library stays the Library).
    fn relayout(&mut self) {
        if self.arr.folder.is_some() {
            self.close_folder();
        }
        let was_lib = self.on_lib();
        self.arrange_apps();
        self.drop_cells();
        let page = match (was_lib, self.lib_page()) {
            (true, Some(l)) => l,
            _ => self.page.min(self.pages_n() - 1),
        };
        if !self.searching {
            self.sel = None;
        }
        self.page = page;
        self.go_page(page, false);
        if self.searching {
            // The results show the change too (dimmed when hidden, the dot gone).
            self.sel = self.sel.filter(|&i| i < self.found.len());
            self.bind_results();
        } else if self.focus == Focus::Grid && !self.on_lib() {
            self.sel = self.first_on_page();
        }
        self.update_sel();
        self.update_msg();
        self.publish();
    }

    /// Saves Home as it is now and Launchpad's section, then shows it.
    pub(super) fn commit_home(&mut self) {
        // (A change: what Tidy Home left is gone, and its undo with it.)
        self.org.undo.forget();
        self.arr.saved = layout_store::to_config(&self.layout);
        self.arr.dirty = false;
        self.lp_save();
        self.relayout();
    }

    // ---- the Library page ----------------------------------------------------------

    /// The Library's visuals follow the pages: a Home page's cells left at
    /// its index (a drag's extra page) go, its container is in the strip
    /// (put back if a rebuild took it out) at its page's place, and hidden
    /// while the Library does not show (an icon dragged on Home).
    pub(super) fn sync_lib_view(&mut self) {
        let lp = self.lib_page();
        if let Some(l) = lp {
            if let Some(v) = self.pages.remove(&l) {
                if self.sel_in == Some(Some(l)) {
                    self.detach_sel();
                }
                let _ = self.strip.Children().and_then(|ch| ch.Remove(&v.container));
            }
        }
        let Some(c) = self.lib.view.as_ref().map(|v| v.container.clone()) else { return };
        if c.Parent().is_err() && self.strip.Children().and_then(|ch| ch.InsertAtTop(&c)).is_err() {
            self.lib.view = None;
            return;
        }
        match lp {
            Some(l) => {
                let _ = c.SetOffset(Vector3 { X: self.page_x(l), Y: 0.0, Z: 0.0 });
                let _ = c.SetIsVisible(true);
            }
            None => {
                let _ = c.SetIsVisible(false);
            }
        }
    }

    /// The Library page's visuals exist (made the first time it is reached).
    pub(super) fn ensure_lib(&mut self) {
        self.sync_lib_view();
        if self.lib.view.is_some() {
            return;
        }
        let Some(p) = self.lib_page() else { return };
        let c = self.gfx.compositor.clone();
        let made = (|| -> Result<LibCells> {
            let container = c.CreateContainerVisual()?;
            container.SetOffset(Vector3 { X: self.page_x(p), Y: 0.0, Z: 0.0 })?;
            let ch = container.Children()?;
            let mut tiles = Vec::new();
            for k in 0..self.lib.groups.len() {
                let r = self.lib.tiles.tile(k, self.rtl).offset(self.work.l, self.work.t);
                let n = self.lib.tiles.name(k, self.rtl).offset(self.work.l, self.work.t);
                let tile = c.CreateSpriteVisual()?;
                let s = self.s;
                // (Already on whole pixels, inside the tile laid out: the
                // surface exactly that square, Tiles::edge.)
                let side = self.lib.tiles.edge();
                tile.SetOffset(Vector3 { X: self.snap(r.l), Y: self.snap(r.t), Z: 0.0 })?;
                tile.SetSize(Vector2 { X: side, Y: side })?;
                let name = c.CreateSpriteVisual()?;
                name.SetOffset(Vector3 { X: self.snap(n.l), Y: self.snap(n.t), Z: 0.0 })?;
                name.SetSize(Vector2 { X: (n.w() * s).ceil() / s, Y: (n.h() * s).ceil() / s })?;
                ch.InsertAtTop(&tile)?;
                ch.InsertAtTop(&name)?;
                tiles.push((tile, name));
            }
            self.strip.Children()?.InsertAtTop(&container)?;
            Ok(LibCells { container, tiles })
        })();
        match made {
            Ok(v) => {
                self.lib.view = Some(v);
                self.bind_lib();
            }
            Err(e) => crate::log!("launchpad: the Library page could not be made: {e:?}"),
        }
    }

    /// The apps a tile shows (big, then mini), their launch targets.
    fn tile_apps(&self, k: usize) -> (Vec<&App>, Vec<&App>) {
        let Some(g) = self.lib.groups.get(k) else { return (Vec::new(), Vec::new()) };
        let (big, mini) = library::parts(g.apps.len());
        let apps: Vec<&App> = g.apps.iter().filter_map(|id| self.app_by_id(id)).collect();
        let b = apps.iter().take(big).copied().collect();
        let m = apps.iter().skip(big).take(mini).copied().collect();
        (b, m)
    }

    /// The tiles show their apps' icons and their names (icons not loaded
    /// yet are asked for, the page on screen's first).
    pub(super) fn bind_lib(&mut self) {
        if self.lib.view.is_none() {
            return;
        }
        let src = self.src_px();
        let mut missing: Vec<String> = Vec::new();
        let n = self.lib.groups.len();
        let mut brushes = Vec::with_capacity(n);
        for k in 0..n {
            let (big, mini) = self.tile_apps(k);
            missing.extend(big.iter().chain(mini.iter()).map(|a| a.target()).filter(|t| loader::cached(t, src).is_none()));
            brushes.push((self.category_tile_brush(k), self.category_name_brush(k)));
        }
        if let Some(v) = &self.lib.view {
            for ((tile, name), (tb, nb)) in v.tiles.iter().zip(brushes) {
                if let Some(b) = tb {
                    let _ = tile.SetBrush(&b);
                }
                if let Some(b) = nb {
                    let _ = name.SetBrush(&b);
                }
            }
        }
        let first = self.on_lib();
        let want: Vec<String> = missing.into_iter().filter(|k| first || !self.asked.contains(k)).collect();
        if !want.is_empty() {
            self.asked.extend(want.iter().cloned());
            loader::want(&want, src, self.hwnd, first);
        }
    }

    /// Tile `k`'s picture (kept once every icon on it is loaded).
    fn category_tile_brush(&mut self, k: usize) -> Option<CompositionSurfaceBrush> {
        let (big, mini) = self.tile_apps(k);
        let src = self.src_px();
        let cat = self.lib.groups.get(k)?.cat;
        let side = self.lib.tiles.edge();
        let key = format!(
            "tile\u{1f}{side}\u{1f}{}\u{1f}{}\u{1f}{}",
            cat.key(),
            big.iter().map(|a| format!("{}{}", a.target(), if self.lib.new.contains(&a.id) { "*" } else { "" })).collect::<Vec<_>>().join("\u{1f}"),
            mini.iter().map(|a| a.target()).collect::<Vec<_>>().join("\u{1f}")
        );
        if let Some(b) = self.lib.brushes.get(&key) {
            return Some(b.clone());
        }
        let complete = big.iter().chain(mini.iter()).all(|a| loader::cached(&a.target(), src).is_some());
        let art = TileArt {
            big: big.iter().map(|a| (loader::cached(&a.target(), src).flatten(), self.lib.new.contains(&a.id))).collect(),
            mini: mini.iter().map(|a| loader::cached(&a.target(), src).flatten()).collect(),
        };
        let (s, rtl, look) = (self.s, self.rtl, self.look());
        // (Whole pixels already: rounded, never a pixel more.)
        let px = (side * s).round() as u32;
        if px == 0 {
            return None;
        }
        let surface = self
            .gfx
            .draw(px, px, |dc, ox, oy| unsafe {
                dc.SetTransform(&Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: ox, M32: oy });
                let r = paint_category_tile(dc, side, &art, &look, rtl);
                dc.SetTransform(&Matrix3x2::identity());
                r
            })
            .ok()?;
        let b = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface).ok()?;
        let _ = b.SetStretch(CompositionStretch::Fill);
        if complete {
            self.lib.brushes.insert(key, b.clone());
        }
        Some(b)
    }

    /// Tile `k`'s name under it, as the design kit's category tiles show
    /// it: the category's glyph (16), 8, its name (the ramp's body strong)
    /// and how many apps it holds (body, in the secondary level), centered
    /// as one line.
    fn category_name_brush(&mut self, k: usize) -> Option<CompositionSurfaceBrush> {
        let g = self.lib.groups.get(k)?;
        let (cat, count): (Cat, usize) = (g.cat, g.apps.len());
        // (Hidden on tiles too small for them.)
        let r = self.lib.tiles.name(k, self.rtl);
        if r.h() <= 0.0 || r.w() <= 0.0 {
            return None;
        }
        let key = format!("name\u{1f}{}\u{1f}{}\u{1f}{count}", r.w(), cat.key());
        if let Some(b) = self.lib.brushes.get(&key) {
            return Some(b.clone());
        }
        let (icon, gap) = (size::ICON_S, space::S3);
        // Both laid out as wide as their text: the boxes mirrored below are
        // the text itself (a wider box puts right-to-left text at its right).
        let n = self.tight_layout(&count.to_string(), crate::theme::ramp::BODY.size, false, r.w());
        let nw = n.as_ref().map_or(0.0, |l| l.1);
        let room = (r.w() - icon - gap - if nw > 0.0 { nw + gap } else { 0.0 }).max(1.0);
        let (l, lw, h) = self.tight_layout(t(cat.label_key()), library::NAME_SIZE, true, room)?;
        let (s, ink, dim, rtl) = (self.s, self.ink(), self.dim(), self.rtl);
        let total = icon + gap + lw + if nw > 0.0 { gap + nw } else { 0.0 };
        let x0 = ((r.w() - total) / 2.0).max(0.0);
        let surface = self
            .gfx
            .draw((r.w() * s).ceil() as u32, (r.h() * s).ceil() as u32, |dc, ox, oy| unsafe {
                dc.SetTransform(&Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: ox, M32: oy });
                // Laid out left to right, then mirrored right to left.
                let at = |x: f32, w: f32| if rtl { r.w() - x - w } else { x };
                let res = (|| {
                    crate::glyphs::draw(dc, cat_glyph(cat), at(x0, icon) + icon / 2.0, r.h() / 2.0, icon, ink)?;
                    draw_layout(dc, &l, at(x0 + icon + gap, lw), (r.h() - h) / 2.0, ink)?;
                    if let Some((nl, nw, nh)) = &n {
                        draw_layout(dc, nl, at(x0 + icon + gap + lw + gap, *nw), (r.h() - nh) / 2.0, dim)?;
                    }
                    Ok(())
                })();
                dc.SetTransform(&Matrix3x2::identity());
                res
            })
            .ok()?;
        let b = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface).ok()?;
        let _ = b.SetStretch(CompositionStretch::Fill);
        self.lib.brushes.insert(key, b.clone());
        Some(b)
    }

    /// The theme changed: the tiles are drawn again.
    pub(super) fn lib_restyle(&mut self) {
        self.lib.brushes.clear();
        self.bind_lib();
    }

    /// Icons came: the tiles showing one of them are drawn again.
    pub(super) fn lib_icons_arrived(&mut self, keys: &[String]) {
        if self.lib.view.is_none() {
            return;
        }
        let shows = (0..self.lib.groups.len()).any(|k| {
            let (b, m) = self.tile_apps(k);
            b.iter().chain(m.iter()).any(|a| keys.contains(&a.target()))
        });
        if shows {
            self.bind_lib();
        }
    }

    /// The Library page's tile under (x, y), if any (the Library on screen).
    pub(super) fn tile_hit(&self, x: f32, y: f32) -> Option<Hit> {
        if !self.on_lib() {
            return None;
        }
        let (gx, gy) = self.to_grid(x, y);
        let counts: Vec<usize> = self.lib.groups.iter().map(|g| g.apps.len()).collect();
        self.lib.tiles.hit(gx - self.work.l, gy - self.work.t, self.rtl, &counts).map(|(k, p)| Hit::LibTile(k, p))
    }

    /// A click on tile `k`: a big icon opens its app, the rest the category.
    pub(super) fn lib_click(&mut self, k: usize, part: Part) {
        if self.arr.edit {
            self.end_edit();
        }
        match part {
            Part::Big(j) => {
                let Some(app) = self.tile_apps(k).0.get(j).map(|a| (*a).clone()) else { return };
                self.lib_seen(&app.id);
                self.finish(Some(After::Open(app.target())));
            }
            Part::Rest => self.open_category(k),
        }
    }

    /// The pointer over tile `k`: the keyboard's place follows (as on Home).
    pub(super) fn lib_hover(&mut self, k: usize, again: bool) {
        if self.lib.sel == Some(k) && self.focus == Focus::Grid && !self.by_key && !again {
            return;
        }
        self.lib.sel = Some(k);
        self.by_key = false;
        if self.focus == Focus::Field {
            self.focus = Focus::Grid;
            self.draw_field();
        }
        self.lib_update_sel();
    }

    /// Tile `k` takes the keyboard's place (said aloud).
    pub(super) fn lib_select(&mut self, k: usize) {
        self.lib.sel = Some(k);
        self.focus = Focus::Grid;
        self.lib_update_sel();
        self.draw_field();
        let said = self.tile_said(k);
        self.announce(said);
    }

    /// The selection shape on the selected tile (or hidden).
    pub(super) fn lib_update_sel(&mut self) {
        let shown = self.focus == Focus::Grid && super::select::pointer_shows(self.by_key, self.pointer_out) && self.drag.is_none() && self.arr.folder.is_none();
        let (Some(k), Some(container), Some(p)) = (self.lib.sel.filter(|_| shown), self.lib.view.as_ref().map(|v| v.container.clone()), self.lib_page()) else {
            self.sel_off(self.by_key);
            return;
        };
        let pad = self.lib.tiles.gap / 4.0;
        let r = self.lib.tiles.item(k, self.rtl).offset(self.work.l, self.work.t);
        let cell = Rect::new(r.l - pad, r.t - pad, r.r + pad, r.b);
        // (The keyboard's ring goes round the tile itself.)
        let tile = self.lib.tiles.tile(k, self.rtl).offset(self.work.l, self.work.t);
        self.sel_on(Some(p), &container, cell, tile, library_tile_radius(self.theme.radii().tile, tile.w()));
    }

    /// Keys on the Library page (the tiles). True: ours.
    pub(super) fn lib_key(&mut self, vk: u16) -> bool {
        if !self.on_lib() || self.focus != Focus::Grid || self.arr.folder.is_some() || self.lib.groups.is_empty() {
            return false;
        }
        let sel = self.lib.sel.unwrap_or(0).min(self.lib.groups.len() - 1);
        match vk {
            v if v == VK_LEFT.0 => self.lib_arrow(Arrow::Left),
            v if v == VK_RIGHT.0 => self.lib_arrow(Arrow::Right),
            v if v == VK_UP.0 => self.lib_arrow(Arrow::Up),
            v if v == VK_DOWN.0 => self.lib_arrow(Arrow::Down),
            v if v == VK_RETURN.0 || v == VK_SPACE.0 => {
                self.eat_space = v == VK_SPACE.0;
                self.open_category(sel);
            }
            v if v == VK_APPS.0 || (v == VK_F10.0 && key_down(VK_SHIFT)) => {
                let r = self.lib.tiles.tile(sel, self.rtl).offset(self.work.l, self.work.t);
                self.lib_menu(sel, Part::Rest, (r.cx(), r.cy()), true);
            }
            _ => return false,
        }
        true
    }

    /// An arrow key among the tiles; past the first one back to Home's
    /// last app, up to the field.
    fn lib_arrow(&mut self, a: Arrow) {
        let Some(cur) = self.lib.sel else {
            self.lib_select(0);
            return;
        };
        match library::step(cur, &self.lib.tiles, a, self.rtl) {
            Some(k) => self.lib_select(k),
            None if matches!(a, Arrow::Left | Arrow::Right) && ((a == Arrow::Left) != self.rtl) => {
                let Some(l) = self.lib_page().filter(|&l| l > 0) else { return };
                self.go_page(l - 1, true);
                let last = self.page_len(l - 1);
                if let Some(i) = last.checked_sub(1).and_then(|s| self.index_of_slot(s)) {
                    self.select(i);
                }
            }
            None if a == Arrow::Up => self.set_focus(Focus::Field),
            None => {}
        }
    }

    // ---- menus -----------------------------------------------------------------

    /// A tile's menu: a big icon's is its app's; the rest's opens the
    /// category or the clean-up.
    pub(super) fn lib_menu(&mut self, k: usize, part: Part, at: (f32, f32), keyboard: bool) {
        if self.arr.edit {
            return;
        }
        if let Part::Big(j) = part {
            if let Some(app) = self.tile_apps(k).0.get(j).map(|a| (*a).clone()) {
                self.open_app_menu(app, at, keyboard, false);
            }
            return;
        }
        let Some(g) = self.lib.groups.get(k) else { return };
        let name = t(g.cat.label_key()).to_string();
        let rows: Vec<Option<(String, MenuAct)>> = vec![Some((t("launchpad.menu.open").to_string(), MenuAct::OpenCategory(k)))];
        let _ = keyboard;
        self.lib.sel = Some(k);
        self.lib_update_sel();
        self.announce(name.clone());
        self.ask_menu(rows, App { name, id: String::new() }, at);
    }

    /// The Home and Library rows of `app`'s menu (none while the app list
    /// is incomplete: Home must not be saved from part of it).
    pub(super) fn lib_menu_rows(&self, app: &App) -> Vec<(String, MenuAct)> {
        if app.id.is_empty() || !self.apps_complete || layout_store::needs_migration(&self.lib.cfg) {
            return Vec::new();
        }
        if self.lib.is_hidden(&app.id) {
            return vec![(t("launchpad.menu.unhide").to_string(), MenuAct::Unhide)];
        }
        let mut rows = Vec::new();
        if self.lib.cfg.show_library {
            if layout_store::on_home(&self.layout, &app.id) {
                rows.push((t("launchpad.menu.remove_home").to_string(), MenuAct::RemoveHome));
            } else {
                rows.push((t("launchpad.menu.add_home").to_string(), MenuAct::AddHome));
            }
            rows.push((t("launchpad.menu.move_to").to_string(), MenuAct::MoveTo));
        }
        rows.push((t("launchpad.menu.hide").to_string(), MenuAct::Hide));
        rows
    }

    /// "Move to category": the categories (its submenu's rows), the app's
    /// own checked.
    pub(super) fn category_rows(&self, app: &App) -> Vec<(String, MenuAct, bool)> {
        let hidden = HashSet::new();
        let now = library::place(&app.id, &app.name, &self.lib.cfg.library, &hidden);
        Cat::PICKABLE.into_iter().map(|c| (t(c.label_key()).to_string(), MenuAct::SetCat(c), c == now)).collect()
    }

    /// A Home / Library row of an app's menu was picked.
    pub(super) fn lib_act(&mut self, act: &MenuAct, app: &App) {
        let (id, name) = (app.id.clone(), app.name.clone());
        let pp = self.per_page();
        let (changed, said) = match act {
            MenuAct::AddHome => (layout_store::add_to_home(&mut self.layout, &id, pp), tf("launchpad.done.added", &[("name", &name)])),
            MenuAct::RemoveHome => (layout_store::remove_from_home(&mut self.layout, &id), tf("launchpad.done.removed", &[("name", &name)])),
            MenuAct::Hide => (layout_store::hide(&mut self.layout, &mut self.lib.cfg.hidden, &id), tf("launchpad.done.hidden", &[("name", &name)])),
            MenuAct::Unhide => (layout_store::unhide(&mut self.lib.cfg.hidden, &id), tf("launchpad.done.unhidden", &[("name", &name)])),
            MenuAct::SetCat(c) => {
                (library::set_category(&mut self.lib.cfg.library, &id, &name, *c), tf("launchpad.done.moved", &[("name", &name), ("category", t(c.label_key()))]))
            }
            _ => return,
        };
        // Moved by the user: not new any more.
        let seen = matches!(act, MenuAct::AddHome | MenuAct::SetCat(_)) && self.lib.new.remove(&id) && layout_store::mark_seen(&mut self.lib.cfg.seen, &id);
        if !changed && !seen {
            return;
        }
        self.commit_home();
        self.announce(said);
    }

}

/// Pictures for the design audit (tests only).
#[cfg(test)]
#[path = "applib_harmony.rs"]
mod harmony;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_line_boxes_are_as_wide_as_their_text() {
        // A category name's box is its text (a pixel for rounding), so
        // mirroring it right to left puts the text where it was placed.
        assert_eq!(tight_width(41.3, 200.0), 43.0);
        assert_eq!(tight_width(8.0, 200.0), 9.0, "a one-digit count");
        // Never wider than allowed (a long name is cut with "…" there).
        assert_eq!(tight_width(250.0, 120.0), 120.0);
        assert_eq!(tight_width(0.0, 0.0), 1.0);
    }

    #[test]
    fn strings_exist() {
        for tag in ["en", "ko"] {
            let _l = crate::i18n::test_lock(tag);
            for c in Cat::ALL {
                assert!(crate::i18n::get(c.label_key()).is_some(), "{tag}: {c:?}");
            }
            let a = tf("launchpad.library.a11y", &[("name", "Games"), ("count", "12")]);
            assert!(a.contains("Games") && a.contains("12"), "{tag}: {a}");
            let m = tf("launchpad.done.moved", &[("name", "Git"), ("category", "Dev")]);
            assert!(m.contains("Git") && m.contains("Dev"), "{tag}: {m}");
        }
    }
}
