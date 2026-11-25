//! Launchpad's edit mode on screen, like a phone's home screen (calm, the
//! Android / One UI way: nothing wiggles). A long press lifts an icon and
//! starts edit mode: the page's grid glides a little smaller on a soft
//! plate (animating; else the plate alone). The lifted icon grows a little,
//! casts the shadows of a thing picked up (elevation E3) and follows the
//! pointer 1:1; a faint copy marks where it would go, and the others make
//! room once the pointer stays. In the middle of another icon the target
//! grows and a plate grows behind it (drop: a folder); at a side edge the
//! edge lights and the page turns. Every number is in edit.rs.
//! Folders open in a panel over the grid, where their apps can be dragged
//! out and their name changed. The pure part is in edit.rs, folders.rs and
//! layout_store.rs; this is the window's side of it (a child of view.rs:
//! it works on the overlay's state).
//!
//! Design system v2.1 ("harmony"): every size, corner, fill and length is
//! a token. What changes place or size glides (crate::motion's GLIDE; a
//! small thing's state snaps), what comes and goes fades 90 / 120, the grid
//! behind a folder fades as the plate does; not animating (Pad::animate)
//! nothing moves or grows and the fades are 60 ms straight (12.2). What is
//! drawn is drawn by one function each (paint_panel, paint_done,
//! paint_edit_plate, paint_folder_tile, paint_drop_plate), which the design
//! audit's pictures call too (arrange_harmony.rs).

use std::collections::HashMap;

use windows::core::{Result, HSTRING};
use windows::core::Interface;
use windows::UI::Color;
use windows::UI::Composition::{
    CompositionColorBrush, CompositionRoundedRectangleGeometry, CompositionStretch, CompositionSurfaceBrush, Compositor, ContainerVisual, ShapeVisual, SpriteVisual, Visual, VisualCollection,
};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{ID2D1DeviceContext, ID2D1Factory1, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_ROUNDED_RECT};
use windows::Win32::Graphics::DirectWrite::{
    IDWriteTextLayout, DWRITE_READING_DIRECTION_RIGHT_TO_LEFT, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::UI::Input::Ime::{ImmGetContext, ImmNotifyIME, ImmReleaseContext, CPS_CANCEL, NI_COMPOSITIONSTR};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, VK_APPS, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F10, VK_F2, VK_HOME, VK_LEFT, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};
use windows_numerics::{Matrix3x2, Vector2, Vector3};

use super::super::edit::{self, Aim, Pos};
use super::super::folders;
use super::super::grid::{self, Arrow, Grid, Rect};
use super::super::layout_store::{self, Pages};
use super::super::loader;
use super::{color, d2r, draw_layout, fade, fill, fill_rr, grid_centre, key_down, real, rgba, stroke_rr, After, Cells, Deferred, Focus, Hit, Pad, Press, Tile, View, EDGE_TIMER, HOLD_TIMER, LONG_PRESS_TIMER, SETTLE_TIMER};
use crate::config::LpItem;
use crate::gfx::Gfx;
use crate::i18n::{t, tf};
use crate::motion::Curve;
use crate::search::edit::Field;
use crate::theme::metrics::{size, space};
use crate::theme::{ramp, Elevation, Level, Rgba, Role, State, Theme};

/// A press that moves this far (DIP) lifts the icon in edit mode, and
/// stops a long press from starting it.
const LIFT_START: f32 = 6.0;
/// Done keeps this far from the work area's side: where a big plate's
/// text starts (3.3).
const DONE_SIDE: f32 = folders::PANEL_INK;
/// Room round Done's picture for the keyboard's ring (6.4: 2 out, 2 wide).
const DONE_M: f32 = size::FOCUS_GAP + size::FOCUS_W;
/// A caret's width (as the search field's).
const CARET_W: f32 = 1.5;
/// The lit side edge keeps this far from the screen's edges.
const EDGE_INSET: f32 = size::EDGE_GAP;

/// How the open folder's name shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum NameRow {
    /// As text (a click on it in edit mode makes it a field).
    Plain,
    /// Being changed: a field, its caret at this x from the text's box
    /// (None: the name could not be laid out, the caret in the middle);
    /// `ring`: the keyboard started it.
    Typing { caret: Option<f32>, ring: bool },
}

/// An icon being dragged on the pages.
pub(in super::super) struct IconDrag {
    /// The one under the pointer (its icon is what is dragged).
    item: LpItem,
    /// The other picked items dragged with it (organize.rs), in the pages'
    /// order, and where it goes among them.
    others: Vec<LpItem>,
    lead_at: usize,
    /// Its tile's key (the brush the dragged visual shows).
    key: String,
    /// The page under it and the gap the others make there.
    page: usize,
    gap: usize,
    /// The pointer's offset from the icon's top-left corner.
    grab: (f32, f32),
    start: (f32, f32),
    moved: bool,
    /// The pages as they were (a cancelled drag puts them back).
    before: Pages,
}

impl IconDrag {
    /// Everything dragged, in its order.
    fn all(self) -> Vec<LpItem> {
        let mut v = self.others;
        v.insert(self.lead_at.min(v.len()), self.item);
        v
    }

    /// Can what is dragged go into a folder (apps only)?
    fn apps_only(&self) -> bool {
        matches!(self.item, LpItem::App(_)) && self.others.iter().all(|it| matches!(it, LpItem::App(_)))
    }
}

/// An app being dragged inside the open folder's panel.
pub(in super::super) struct FolderDrag {
    id: String,
    key: String,
    /// The folder's other apps, and the gap among them.
    rest: Vec<String>,
    gap: usize,
    grab: (f32, f32),
    start: (f32, f32),
    moved: bool,
    before: Pages,
    /// Where the pointer was last (a scroll aims again from there).
    last: (f32, f32),
}

/// The folder shown open, or a Library category (the same panel, read
/// only: no renaming, no dragging, its apps by relevance).
pub(in super::super) struct OpenFolder {
    /// Where it is on the pages (a category: nowhere, never read).
    pub at: Pos,
    /// A Library category, and its tile's index.
    pub cat: Option<(super::super::library::Cat, usize)>,
    /// Its panel's grid (relative to the work area, like the main grid).
    pub pg: Grid,
    /// Rows scrolled (a big folder).
    pub scroll: usize,
    /// The app with the keyboard focus (index in the folder).
    pub sel: Option<usize>,
    pub drag: Option<FolderDrag>,
}

pub(in super::super) struct Arr {
    /// Edit mode is on.
    pub edit: bool,
    /// The arrangement saved in the config (when opened, or as saved since).
    pub saved: Vec<Vec<LpItem>>,
    /// The pages changed since they were last saved.
    pub dirty: bool,
    drag: Option<IconDrag>,
    /// The keyboard lifted the icon here (edit mode).
    lifted: Option<Pos>,
    /// The press armed for edit mode (LONG_PRESS_TIMER).
    long_press: bool,
    /// A dragged icon is in the middle of item `hold` of its page (the
    /// folder intent, shown), and has been for FOLDER_DWELL_MS (HOLD_TIMER:
    /// dropping makes a folder).
    hold: Option<usize>,
    hold_ready: bool,
    /// The target icon grown by the folder intent.
    target: Option<SpriteVisual>,
    /// The gap waiting to move here (SETTLE_TIMER), for drag number
    /// `pending_gen` (a wait outliving its drag does nothing).
    pending: Option<usize>,
    pending_gen: u64,
    /// Bumped whenever a drag starts or ends.
    drag_gen: u64,
    /// The grid's scale change under way: from this, started then, this
    /// long (ms as shown); input reads the scale shown at each moment.
    scale_from: f32,
    scale_t0: Option<std::time::Instant>,
    scale_len: i64,
    /// Where the folder plate is (container DIP).
    plate_rect: Rect,
    /// A dragged icon rests at a side edge (EDGE_TIMER): toward the next page?
    edge: Option<bool>,
    /// Edit mode's grid scale now (edit::edit_scale; 1 outside edit mode).
    pub scale: f32,
    /// The open folder's border is lit (an app dragged near it).
    border_lit: bool,
    /// The tile just dropped and where it was let go (it slides from there).
    pub drop_at: Option<(String, Vector3)>,
    pub folder: Option<OpenFolder>,
    pub folder_view: Option<View>,
    /// The open folder's scroll edge, while it holds more rows than its
    /// panel shows (folder_bands).
    folder_edges: Option<FolderEdges>,
    pub folder_brushes: HashMap<String, CompositionSurfaceBrush>,
    /// The open folder's name being changed.
    pub rename: Option<Field>,
    /// The keyboard started the rename (F2): its field carries the ring.
    rename_ring: bool,
    /// The rename ended with an IME composition still open: it was folded
    /// into the name (or dropped) and cancelled; what the IME still sends
    /// for it is ignored until the next composition.
    pub ime_dropped: bool,
    /// The folder plate behind a target, the faint copy where a dragged icon
    /// would go (both in the page's container, so they scale with it), the
    /// lit edge, edit mode's plate (under the grid).
    plate_v: SpriteVisual,
    ghost_v: SpriteVisual,
    edge_v: SpriteVisual,
    page_v: SpriteVisual,
    /// Edit mode's plate was left fading out (edit_look): shown again,
    /// it fades back from where it is.
    page_out: bool,
    folder_box: ContainerVisual,
    panel_v: SpriteVisual,
    /// A closed folder's panel fading out, over the box; dropped when the
    /// next one opens or closes.
    leaving: Option<ContainerVisual>,
    /// Done: its fill (a brush that fades between its states) under its
    /// picture (the contact shadow, the label, the keyboard's ring).
    done_fill: ShapeVisual,
    done_geom: CompositionRoundedRectangleGeometry,
    done_brush: CompositionColorBrush,
    done_v: SpriteVisual,
    /// Rest, Hover (the pointer is on it) or Pressed.
    done_state: State,
    /// Its picture carries the keyboard's ring.
    done_ring: bool,
    /// The dragged icon: what follows the pointer (its place and size),
    /// holding the shadows it casts (elevation E3, shaped like the icon)
    /// under the icon itself.
    drag_v: ContainerVisual,
    drag_shade: ContainerVisual,
    drag_icon: SpriteVisual,
    /// The dragged icon's edge (DIP): the icon it was lifted from as drawn
    /// (grid icons shrink on a small work area).
    drag_e: f32,
    /// Where Done is (monitor DIP; empty out of edit mode).
    pub done_rect: Rect,
}

impl Arr {
    pub fn new(c: &Compositor) -> Result<Self> {
        let sprite = || -> Result<SpriteVisual> {
            let v = c.CreateSpriteVisual()?;
            v.SetIsVisible(false)?;
            Ok(v)
        };
        let folder_box = c.CreateContainerVisual()?;
        folder_box.SetIsVisible(false)?;
        let panel_v = sprite()?;
        folder_box.Children()?.InsertAtTop(&panel_v)?;
        let drag_v = c.CreateContainerVisual()?;
        drag_v.SetIsVisible(false)?;
        let drag_shade = c.CreateContainerVisual()?;
        let drag_icon = c.CreateSpriteVisual()?;
        drag_v.Children()?.InsertAtTop(&drag_shade)?;
        drag_v.Children()?.InsertAtTop(&drag_icon)?;
        let done_fill = c.CreateShapeVisual()?;
        done_fill.SetIsVisible(false)?;
        let done_geom = c.CreateRoundedRectangleGeometry()?;
        let done_brush = c.CreateColorBrushWithColor(Color { A: 0, R: 0, G: 0, B: 0 })?;
        let done_shape = c.CreateSpriteShapeWithGeometry(&done_geom)?;
        done_shape.SetFillBrush(&done_brush)?;
        done_fill.Shapes()?.Append(&done_shape)?;
        Ok(Arr {
            edit: false,
            saved: Vec::new(),
            dirty: false,
            drag: None,
            lifted: None,
            long_press: false,
            hold: None,
            hold_ready: false,
            target: None,
            pending: None,
            pending_gen: 0,
            drag_gen: 0,
            scale_from: 1.0,
            scale_t0: None,
            scale_len: 0,
            plate_rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            edge: None,
            scale: 1.0,
            border_lit: false,
            drop_at: None,
            folder: None,
            folder_view: None,
            folder_edges: None,
            folder_brushes: HashMap::new(),
            rename: None,
            rename_ring: false,
            ime_dropped: false,
            plate_v: sprite()?,
            ghost_v: sprite()?,
            edge_v: sprite()?,
            page_v: sprite()?,
            page_out: false,
            folder_box,
            panel_v,
            leaving: None,
            done_fill,
            done_geom,
            done_brush,
            done_v: sprite()?,
            done_state: State::Rest,
            done_ring: false,
            drag_v,
            drag_shade,
            drag_icon,
            drag_e: grid::ICON,
            done_rect: Rect::new(0.0, 0.0, 0.0, 0.0),
        })
    }

    /// Edit mode's panel under the grid; over it the lit edge and the
    /// open folder.
    pub fn insert_low(&self, ch: &VisualCollection, grid_box: &ContainerVisual) -> Result<()> {
        ch.InsertBelow(&self.page_v, grid_box)?;
        ch.InsertAtTop(&self.edge_v)?;
        ch.InsertAtTop(&self.folder_box)
    }

    /// Over the rest (below the card): Done, the dragged icon.
    pub fn insert_high(&self, ch: &VisualCollection) -> Result<()> {
        ch.InsertAtTop(&self.done_fill)?;
        ch.InsertAtTop(&self.done_v)?;
        ch.InsertAtTop(&self.drag_v)
    }

    /// An icon is being dragged on the pages.
    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// The gap a dragged icon makes on page `p`.
    pub fn gap_on(&self, p: usize) -> Option<usize> {
        self.drag.as_ref().filter(|d| d.page == p).map(|d| d.gap)
    }
}

/// ---- motion: crate::motion's values on Composition animations ------------------

/// The GLIDE as the Composition animations play it: (progress, share of
/// the way done) keys, straight between (edit::glide_at reads the same).
fn glide_keys() -> Vec<(f32, f32)> {
    crate::motion::SPRING_GLIDE.keys(crate::motion::GLIDE_MAX_MS, crate::motion::SAMPLES)
}

/// How long a GLIDE plays at the animation speed (ms of real time).
fn glide_ms() -> i64 {
    crate::effects::ms(crate::motion::GLIDE_MAX_MS)
}

fn v3(k: f32) -> Vector3 {
    Vector3 { X: k, Y: k, Z: 1.0 }
}

/// `v`'s Vector3 property `prop` ("Offset", "Scale") glides to `to` (7.4:
/// a thing's place or size changes): from `from` if given, else from where
/// it is now (its value is carried over, not its speed).
fn glide(c: &Compositor, v: &impl Interface, prop: &str, from: Option<Vector3>, to: Vector3) -> Result<()> {
    let v: Visual = v.cast()?;
    if prop == "Scale" {
        // (A spring on its parts, the folder intent's, must not run under it.)
        for part in ["Scale.X", "Scale.Y"] {
            let _ = v.StopAnimation(&HSTRING::from(part));
        }
    }
    let lin = c.CreateLinearEasingFunction()?;
    let a = c.CreateVector3KeyFrameAnimation()?;
    match from {
        Some(f) => {
            for (p, done) in glide_keys() {
                let at = Vector3 { X: f.X + (to.X - f.X) * done, Y: f.Y + (to.Y - f.Y) * done, Z: f.Z + (to.Z - f.Z) * done };
                a.InsertKeyFrameWithEasingFunction(p, at, &lin)?;
            }
        }
        None => {
            a.SetVector3Parameter(&HSTRING::from("to"), to)?;
            for (p, done) in glide_keys().into_iter().skip(1) {
                a.InsertExpressionKeyFrameWithEasingFunction(p, &HSTRING::from(format!("this.StartingValue + (to - this.StartingValue) * {done:.4}")), &lin)?;
            }
        }
    }
    a.SetDuration(real(glide_ms()))?;
    v.StartAnimation(&HSTRING::from(prop), &a)
}

/// `v`'s opacity rides the same GLIDE to `to` (what a glide brings with
/// it: a lifted icon's shadows), from `from` or from where it is.
fn glide_opacity(c: &Compositor, v: &impl Interface, from: Option<f32>, to: f32) -> Result<()> {
    let v: Visual = v.cast()?;
    let lin = c.CreateLinearEasingFunction()?;
    let a = c.CreateScalarKeyFrameAnimation()?;
    match from {
        Some(f) => {
            for (p, done) in glide_keys() {
                a.InsertKeyFrameWithEasingFunction(p, (f + (to - f) * done).clamp(0.0, 1.0), &lin)?;
            }
        }
        None => {
            a.SetScalarParameter(&HSTRING::from("to"), to)?;
            for (p, done) in glide_keys().into_iter().skip(1) {
                a.InsertExpressionKeyFrameWithEasingFunction(p, &HSTRING::from(format!("this.StartingValue + (to - this.StartingValue) * {:.4}", done.clamp(0.0, 1.0))), &lin)?;
            }
        }
    }
    a.SetDuration(real(glide_ms()))?;
    v.StartAnimation(&HSTRING::from("Opacity"), &a)
}

/// A tile's visual `v`, bound to another cell in edit mode, comes to its
/// place `to` from `from`: gliding there (`glides`: Launchpad animates),
/// else it is there at once and comes in over REDUCED_MS (12.2: nothing
/// slides).
pub(in super::super) fn slide(c: &Compositor, v: &SpriteVisual, from: Vector3, to: Vector3, glides: bool) -> Result<()> {
    if glides {
        return glide(c, v, "Offset", Some(from), to);
    }
    let shown = v.Opacity().unwrap_or(1.0);
    let plan = crate::motion::fade_plan(crate::motion::Mode::Reduced, crate::motion::REDUCED_MS, crate::motion::LINEAR, 1.0);
    v.StartAnimation(&HSTRING::from("Opacity"), &fade(c, Some(0.0), shown, plan)?)
}

/// `v`'s opacity to `to` as `plan` says (crate::motion::fade_plan's
/// answer), from `from` or from where it is (a fade under way turns back
/// from there). A plan of no length, or an animation that cannot start: it
/// is there at once. True: it fades.
fn fade_v(c: &Compositor, v: &impl Interface, from: Option<f32>, to: f32, plan: (i64, Curve)) -> bool {
    let Ok(v) = v.cast::<Visual>() else { return false };
    let prop = HSTRING::from("Opacity");
    let fades = plan.0 > 0 && fade(c, from, to, plan).and_then(|a| v.StartAnimation(&prop, &a)).is_ok();
    if !fades {
        let _ = v.StopAnimation(&prop);
        let _ = v.SetOpacity(to);
    }
    fades
}

/// Brush `b`'s color to `to` as `plan` says, from `from` or from the color
/// it shows (a fill that fades between a thing's states; plan of no
/// length: at once).
pub(super) fn color_to(c: &Compositor, b: &CompositionColorBrush, from: Option<Color>, to: Color, plan: (i64, Curve)) {
    let prop = HSTRING::from("Color");
    let fades = plan.0 > 0
        && (|| -> Result<()> {
            let a = c.CreateColorKeyFrameAnimation()?;
            if let Some(f) = from {
                a.InsertKeyFrame(0.0, f)?;
            }
            a.InsertKeyFrameWithEasingFunction(1.0, to, &crate::motion::easing(c, plan.1)?)?;
            a.SetDuration(real(plan.0))?;
            b.StartAnimation(&prop, &a)
        })()
        .is_ok();
    if !fades {
        let _ = b.StopAnimation(&prop);
        let _ = b.SetColor(to);
    }
}

/// Pure: where edit mode's plate fades in from: `fresh` (not left fading
/// out) from nothing, else from where it shows now (None), so a quick
/// return does not drop it first. (Its fade out always goes on from where
/// it shows: a quick exit does not flash it full first.)
fn ring_from(fresh: bool) -> Option<f32> {
    fresh.then_some(0.0)
}

/// Sets `v`'s scale at once (any animation of it stopped).
fn set_scale(v: &impl Interface, k: f32) {
    if let Ok(v) = v.cast::<Visual>() {
        for prop in ["Scale", "Scale.X", "Scale.Y"] {
            let _ = v.StopAnimation(&HSTRING::from(prop));
        }
        let _ = v.SetScale(v3(k));
    }
}

fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

// ---- what is drawn: one function each (the audit's pictures call them too) -----

/// A state's line in high contrast, and the lit border of the open
/// folder (12.3: a boundary is 1 wide, a state 2).
const STATE_LINE: f32 = 2.0;

/// `rect` (corners `radius`) as a stroke `lw` wide lying inside its edge.
pub(super) fn inner(rect: D2D_RECT_F, radius: f32, lw: f32) -> D2D1_ROUNDED_RECT {
    let h = lw / 2.0;
    let rad = (radius - h).max(0.0);
    D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: rect.left + h, top: rect.top + h, right: rect.right - h, bottom: rect.bottom - h }, radiusX: rad, radiusY: rad }
}

/// The icons' own shape (the dock's squircle; 1.3) `side` wide at (`x`,
/// `y`): filled with `fill`, or outlined (high contrast's `line`: color
/// and width, inside its edge).
pub(super) unsafe fn squircle(dc: &ID2D1DeviceContext, x: f32, y: f32, side: f32, fill: Rgba, line: Option<(Rgba, f32)>) -> Result<()> {
    let factory: ID2D1Factory1 = dc.GetFactory()?.cast()?;
    if let Some((c, w)) = line {
        return crate::gfx::parts::paint_squircle_ring(dc, &factory, x, y, side, w, c);
    }
    if fill.a > 0 {
        let geom = crate::gfx::squircle_geometry(&factory, x, y, side)?;
        dc.FillGeometry(&geom, &dc.CreateSolidColorBrush(&rgba(fill), None)?, None);
    }
    Ok(())
}

/// A raised ground in the icons' own shape `side` wide at (`x`, `y`) that
/// fills its own picture (a folder's tile, an icon's place until it is
/// loaded): the raised fill at rest with Flat v2's card light round it,
/// drawn a hair in from the picture's edge so its drop fits
/// (gfx::parts::card_room); high contrast: its outline (`line`, as
/// squircle). `k`: `dc`'s units per DIP.
#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn raised_squircle(dc: &ID2D1DeviceContext, x: f32, y: f32, side: f32, theme: &Theme, line: Option<(Rgba, f32)>, k: f32) -> Result<()> {
    let fill = theme.fill_of(Role::Raised, State::Rest);
    if line.is_some() || theme.card_fx(State::Rest).is_none() {
        return squircle(dc, x, y, side, fill, line);
    }
    let factory: ID2D1Factory1 = dc.GetFactory()?.cast()?;
    let room = crate::gfx::parts::card_room(theme, State::Rest, D2D_RECT_F { left: x, top: y, right: x + side, bottom: y + side }, k);
    let (sx, sy) = ((room.right - room.left) / side, (room.bottom - room.top) / side);
    let full = crate::gfx::squircle_geometry(&factory, x, y, side)?;
    let shape = factory.CreateTransformedGeometry(&full, &Matrix3x2 { M11: sx, M12: 0.0, M21: 0.0, M22: sy, M31: room.left - sx * x, M32: room.top - sy * y })?;
    let shape: windows::Win32::Graphics::Direct2D::ID2D1Geometry = shape.cast()?;
    crate::gfx::parts::card_under_shape(dc, &shape, theme, State::Rest, k, 1.0)?;
    dc.FillGeometry(&shape, &dc.CreateSolidColorBrush(&rgba(fill), None)?, None);
    crate::gfx::parts::card_over_shape(dc, &shape, theme, State::Rest, k, 1.0)
}

/// Where an icon will be on a tile, until it is loaded: ink on the tile's
/// ground (the row's fill under the pointer: it shows on a raised fill in
/// both palettes).
pub(super) fn slot_fill(theme: &Theme) -> Rgba {
    theme.fill_of(Role::Row, State::Hover)
}

/// A folder's tile `e` DIP wide: its ground in the icons' own shape, an
/// icon's whole edge (5: the raised fill at rest; high contrast: its
/// outline), and its first apps' icons in a grid on it (None: not loaded
/// yet, the icons' shape in slot_fill).
pub(super) unsafe fn paint_folder_tile(dc: &ID2D1DeviceContext, e: f32, minis: &[Option<std::sync::Arc<crate::icons::Icon>>], theme: &Theme, rtl: bool) -> Result<()> {
    dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
    let line = theme.outline_of(Role::Raised, State::Rest);
    // (A raised thing: Flat v2's card light round it, inside the tile.)
    raised_squircle(dc, 0.0, 0.0, e, theme, line, 1.0)?;
    for (k, icon) in minis.iter().enumerate() {
        let r = folders::mini_rect(k, e, rtl);
        match icon {
            Some(i) => super::applib::paint_icon(dc, i, r)?,
            None => squircle(dc, r.l, r.t, r.w(), slot_fill(theme), line)?,
        }
    }
    Ok(())
}

/// The plate behind the icon a dragged one is held over (drop: a folder),
/// `side` DIP wide, in the icons' own shape: ink laid on the plate under
/// it (6.2: under the pointer, then, `ready` to make the folder, the
/// selected fill; high contrast: Highlight's outline, then its fill).
pub(super) unsafe fn paint_drop_plate(dc: &ID2D1DeviceContext, side: f32, theme: &Theme, ready: bool) -> Result<()> {
    dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
    let s = if ready { State::Selected } else { State::Hover };
    squircle(dc, 0.0, 0.0, side, theme.fill_of(Role::Row, s), theme.outline_of(Role::Row, s))
}

/// Edit mode's plate `w` x `h` under the grid: a big plate's corners
/// (r.sheet) and the raised fill at rest; high contrast: its outline.
pub(super) unsafe fn paint_edit_plate(dc: &ID2D1DeviceContext, w: f32, h: f32, theme: &Theme) -> Result<()> {
    let rect = D2D_RECT_F { left: 0.0, top: 0.0, right: w, bottom: h };
    let rad = theme.radii().sheet.min(w.min(h) / 2.0);
    match theme.outline_of(Role::Raised, State::Rest) {
        Some((c, lw)) => stroke_rr(dc, rgba(c), &inner(rect, rad, lw), lw),
        // (Raised: Flat v2's card light round it, inside its picture.)
        None => crate::gfx::parts::card(dc, crate::gfx::parts::card_room(theme, State::Rest, rect, 1.0), rad, theme, State::Rest, theme.fill_of(Role::Raised, State::Rest), 1.0, 1.0),
    }
}

/// The side edge a dragged icon rests at, lit (`w` x `h`: the edge's
/// band): the selected fill inside EDGE_INSET, a card's corners; high
/// contrast: Highlight's outline.
pub(super) unsafe fn paint_edge(dc: &ID2D1DeviceContext, w: f32, h: f32, theme: &Theme) -> Result<()> {
    let rect = D2D_RECT_F { left: EDGE_INSET, top: EDGE_INSET, right: w - EDGE_INSET, bottom: h - EDGE_INSET };
    let rad = theme.radii().card;
    match theme.outline_of(Role::Row, State::Hover) {
        Some((c, lw)) => stroke_rr(dc, rgba(c), &inner(rect, rad, lw), lw),
        None => fill_rr(dc, rgba(theme.fill_of(Role::Row, State::Selected)), &D2D1_ROUNDED_RECT { rect, radiusX: rad, radiusY: rad }),
    }
}

/// Pure: Done's size for a label `tw` wide: a control's height, the label
/// a button's padding from its ends.
pub(super) fn done_size(tw: f32) -> (f32, f32) {
    ((tw + size::BUTTON_PAD_X * 2.0).ceil(), size::CONTROL_H)
}

/// Pure: where Done (`w` wide) is on work area `work`: at its trailing
/// side, level with the search field's middle.
fn done_place(work: &Rect, w: f32, rtl: bool) -> Rect {
    let x = if rtl { work.l + DONE_SIDE } else { work.r - DONE_SIDE - w };
    let top = work.t + grid::FIELD_TOP + (grid::FIELD_H - size::CONTROL_H) / 2.0;
    Rect::new(x, top, x + w, top + size::CONTROL_H)
}

/// Edit mode's Done `w` x `h` from the top left (1.1: a button that sits
/// in nothing is a capsule; 6.1: a raised thing): its contact shadow, its
/// label (`label`: its layout in Body Strong, width and height) and, with
/// `focus`, the keyboard's ring. `filled`: its fill in `state` as well (on
/// screen that is a brush of its own, which fades between the states).
/// High contrast: its outline, Highlight's under the pointer, Highlight
/// filled when pressed.
#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn paint_done(dc: &ID2D1DeviceContext, w: f32, h: f32, label: (&IDWriteTextLayout, f32, f32), theme: &Theme, state: State, filled: bool, focus: bool) -> Result<()> {
    let rect = D2D_RECT_F { left: 0.0, top: 0.0, right: w, bottom: h };
    let rad = crate::theme::capsule(h);
    crate::gfx::parts::contact(dc, rect, rad, theme.contact(), 1.0)?;
    // Flat v2's card light: the drop outside it, the light along its top
    // (gone pressed) over its fill; on screen the fill is a visual of its
    // own under this picture (card_under draws only outside the shape).
    crate::gfx::parts::card_under(dc, rect, rad, theme, state, 1.0, 1.0)?;
    if filled {
        fill_rr(dc, rgba(theme.fill_of(Role::Raised, state)), &D2D1_ROUNDED_RECT { rect, radiusX: rad, radiusY: rad })?;
    }
    crate::gfx::parts::card_over(dc, rect, rad, theme, state, 1.0, 1.0)?;
    if let Some((c, lw)) = theme.outline_of(Role::Raised, state) {
        stroke_rr(dc, rgba(c), &inner(rect, rad, lw), lw)?;
    }
    let (l, tw, th) = label;
    draw_layout(dc, l, ((w - tw) / 2.0).round(), ((h - th) / 2.0).round(), rgba(theme.text_of(Level::Primary, state)))?;
    if focus {
        crate::gfx::parts::focus_ring(dc, rect, rad, theme, 1.0)?;
    }
    Ok(())
}

/// The open folder's panel's surface: the menus' and panels' tint, opaque
/// (it has no blur of its own over Launchpad's); the Window color in high
/// contrast.
fn panel_fill(theme: &Theme) -> Rgba {
    match theme.hc.filter(|_| theme.high_contrast) {
        Some(h) => h.window,
        None => Rgba { a: 255, ..theme.popup_tint() },
    }
}

/// Pure: what the open folder's (or category's) panel wears on a display
/// scaled `s` (paint_panel draws it): a panel's plate (Theme::plate_coat:
/// Flat v2's fall, its grain on the Flat one chose, its rim 1 DIP; one
/// device pixel elsewhere), opaque.
pub(super) fn panel_coat(theme: &Theme, s: f32) -> crate::theme::PlateCoat {
    theme.plate_coat(crate::theme::Material::Panel, panel_fill(theme), s)
}

/// The open folder's name laid out: the ramp's title (its optical size and
/// tracking: Gfx::text_style), one line centered in `max_w`, cut with "…".
/// With its height.
pub(super) fn name_layout(gfx: &Gfx, dw: &windows::Win32::Graphics::DirectWrite::IDWriteFactory, theme: &Theme, text: &str, max_w: f32, rtl: bool) -> Option<(IDWriteTextLayout, f32)> {
    let (l, _, h) = gfx.text_style(text, &theme.font.family, folders::TITLE, 1.0).ok()?;
    unsafe {
        let _ = l.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
        let _ = l.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
        if rtl {
            let _ = l.SetReadingDirection(DWRITE_READING_DIRECTION_RIGHT_TO_LEFT);
        }
        if let Ok(sign) = dw.CreateEllipsisTrimmingSign(&l) {
            let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
            let _ = l.SetTrimming(&trim, &sign);
        }
        l.SetMaxWidth(max_w.max(1.0)).ok()?;
    }
    Some((l, h))
}

/// What the open folder's panel shows besides its apps' cells.
pub(super) struct PanelLook<'a> {
    /// The name (name_layout) and its height.
    pub name: Option<(&'a IDWriteTextLayout, f32)>,
    pub row: NameRow,
    /// An app is dragged near the border: one step more and it is out.
    pub lit: bool,
    /// Nothing in it: the quiet block's line.
    pub empty: Option<&'a str>,
    /// The "Shadows" effect is on.
    pub shadows: bool,
    /// What the plate wears (panel_coat): its fall, its grain, its rim
    /// and how wide.
    pub coat: crate::theme::PlateCoat,
}

/// The scroll edge of a folder taller than its panel (design system 2.5:
/// the name's row is the header; no foot, so the bottom is just cut): its
/// cells' view is shown through popup::parts::Edges, the menus' own (a
/// band of alpha on the content under the name's row while rows are
/// scrolled away above; high contrast, a 1 DIP line there).
pub(super) struct FolderEdges {
    edges: crate::popup::parts::Edges,
    /// What holds the cells' view (Edges scales it to px and shows it).
    viewport: ContainerVisual,
}

/// Pure: where a folder panel's apps scroll (relative to the work area):
/// from under the name's row (the header: what scrolls is cut there) to
/// the panel's bottom edge, its full width.
pub(super) fn folder_scroll_area(pg: &Grid) -> Rect {
    let p = folders::panel_rect(pg);
    Rect::new(p.l, pg.top, p.r, p.b)
}

/// Pure: the scroll edge's bands (top, bottom) for a folder of `n` apps
/// in panel grid `pg` scrolled `scroll` rows. None: it shows all its rows
/// (nothing scrolls: no edge). The top fades only while rows are hidden
/// above; there is no fixed foot, so never the bottom.
pub(super) fn folder_bands(n: usize, pg: &Grid, scroll: usize) -> Option<(bool, bool)> {
    (folders::max_scroll(n, pg) > 0).then_some((scroll > 0, false))
}

/// Shows the cells' view `cells` (laid out in the overlay's DIP) through
/// a scroll edge over `area` (DIP, on whole px at `s`), bands `bands`.
/// `cells` must not have a parent; its offset moves it into the area.
pub(super) fn folder_edges(gfx: &Gfx, theme: &Theme, cells: &ContainerVisual, area: Rect, s: f32, bands: (bool, bool)) -> Result<FolderEdges> {
    let viewport = gfx.compositor.CreateContainerVisual()?;
    cells.SetOffset(Vector3 { X: -area.l, Y: -area.t, Z: 0.0 })?;
    viewport.Children()?.InsertAtTop(cells)?;
    let edges = crate::popup::parts::Edges::new(gfx, theme, &viewport, (area.w(), area.h(), size::SCROLL_FADE), s, bands)?;
    edges.stage.SetOffset(Vector3 { X: area.l, Y: area.t, Z: 0.0 })?;
    Ok(FolderEdges { edges, viewport })
}

/// Pure: `r` on whole device pixels at `s` px a DIP (a picture of a
/// visual shown 1:1 stays sharp).
fn on_px(r: Rect, s: f32) -> Rect {
    let px = |v: f32| (v * s).round() / s;
    Rect::new(px(r.l), px(r.t), px(r.r), px(r.b))
}

/// The open folder's (or category's) panel `w` x `h` from the top left: a
/// big plate (1.2: r.sheet; 2.3: elevation E3, its contact shadow and its
/// rim; high contrast: a 1 DIP outline instead). Its name's row is 16 in
/// and a field's height, the name itself the ramp's title starting 28 in
/// (3.3); while the name is changed the row is a field (a well), with the
/// keyboard's ring when the keyboard started it. Empty: the quiet block
/// (11.1).
pub(super) unsafe fn paint_panel(gfx: &Gfx, dc: &ID2D1DeviceContext, w: f32, h: f32, theme: &Theme, look: &PanelLook) -> Result<()> {
    dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
    let rect = D2D_RECT_F { left: 0.0, top: 0.0, right: w, bottom: h };
    let rad = theme.radii().sheet.min(w.min(h) / 2.0);
    if look.shadows {
        crate::gfx::soft_shadows(dc, rect, rad, &theme.elevation(Elevation::E3))?;
    }
    crate::gfx::parts::contact(dc, rect, rad, theme.contact(), 1.0)?;
    // The panels' plate (panel_coat): Flat v2's fall from top to bottom
    // and its grain (drawn into the one picture with it), the rim.
    let coat = &look.coat;
    let fall = (coat.fill.0 != coat.fill.1).then_some(coat.fill);
    crate::gfx::parts::plate_fill(dc, rect, rad, fall, coat.fill.0)?;
    crate::gfx::parts::grain(dc, rect, rad, coat.grain)?;
    if let Some((stops, px)) = coat.rim {
        crate::gfx::parts::rim(dc, rect, rad, stops, px)?;
    }
    // Its edge: none but high contrast's outline; lit, a state's line
    // (the selected ink; Highlight in high contrast).
    let edge = match look.lit {
        true => theme.outline_of(Role::Row, State::Hover).or(Some((theme.fill_of(Role::Row, State::Selected), STATE_LINE))),
        false => theme.outline_of(Role::Raised, State::Rest),
    };
    if let Some((c, lw)) = edge {
        stroke_rr(dc, rgba(c), &inner(rect, rad, lw), lw)?;
    }
    let row = folders::name_rect(w);
    let row_rad = crate::theme::concentric(theme.radii().sheet, folders::PANEL_PAD);
    if look.row != NameRow::Plain {
        crate::gfx::parts::well(dc, d2r(row), row_rad, theme, 1.0)?;
    }
    if matches!(look.row, NameRow::Typing { ring: true, .. }) {
        crate::gfx::parts::focus_ring(dc, d2r(row), row_rad, theme, 1.0)?;
    }
    let ink = rgba(theme.text_of(Level::Primary, State::Rest));
    let caret = |x: f32, top: f32, bottom: f32| fill(dc, ink, Rect::new(x, top + space::S1, x + CARET_W, bottom - space::S1));
    match (look.name, look.row) {
        (Some((l, th)), row_look) => {
            // (The layout centres the text in its box: positions in it
            // include that.)
            let (x, y) = (folders::PANEL_INK, row.cy() - th / 2.0);
            draw_layout(dc, l, x, y, ink)?;
            if let NameRow::Typing { caret: Some(cx), .. } = row_look {
                caret(x + cx, y, y + th)?;
            }
        }
        (None, NameRow::Typing { .. }) => caret(w / 2.0, row.cy() - folders::TITLE.line / 2.0, row.cy() + folders::TITLE.line / 2.0)?,
        _ => {}
    }
    if let Some(text) = look.empty {
        let area = D2D_RECT_F { left: 0.0, top: folders::PANEL_TITLE, right: w, bottom: h - folders::PANEL_PAD };
        gfx.quiet(dc, area, crate::gfx::parts::Quiet::Empty, crate::glyphs::Glyph::Folder, text, theme, 1.0, 0.0)?;
    }
    Ok(())
}

impl Pad {
    // ---- edit mode ------------------------------------------------------------

    /// Can the pages be arranged now? Not while searching, and only with
    /// the whole app list read (an incomplete list must never be saved as
    /// the arrangement: the apps it lacks would lose their places).
    /// (By the snapshot the pages show: view::arrange_ok.)
    pub(super) fn can_arrange(&self) -> bool {
        super::arrange_ok(self.apps_complete, self.searching, self.apps.len())
    }

    /// Edit mode's motion (the grid gliding smaller, icons growing and
    /// gliding): Launchpad animates and the small animations are on.
    fn calm_motion(&self) -> bool {
        self.animate() && crate::effects::items().small
    }

    /// How something of edit mode comes (`on`) or goes: 90 in, 120 out
    /// (6.3); not animating 60 ms straight; at once in high contrast.
    fn fx(&self, on: bool) -> (i64, Curve) {
        super::motion::pointer(self.animate(), on, self.hc().is_some(), crate::effects::current().speed)
    }

    /// How the folder's panel fades in or out: as a panel does (180 in,
    /// 120 out); not animating 60 ms straight.
    fn panel_fade(&self, opening: bool) -> (i64, Curve) {
        use crate::motion::{CLOSE_MS, EASE_IN, EASE_OUT, PANEL_FADE_MS};
        let (ms, curve) = if opening { (PANEL_FADE_MS, EASE_OUT) } else { (CLOSE_MS, EASE_IN) };
        crate::motion::fade_plan(super::motion::mode(self.animate()), ms, curve, crate::effects::current().speed)
    }

    /// Draws into `v` at `r` with `m` DIP of room round it (shadows, a
    /// ring): `f` paints from `r`'s top left.
    fn paint_with_room(&self, v: &SpriteVisual, r: Rect, m: f32, f: impl FnOnce(&ID2D1DeviceContext) -> Result<()>) {
        self.paint_into(v, Rect::new(r.l - m, r.t - m, r.r + m, r.b + m), |dc| unsafe {
            let mut t = Matrix3x2::default();
            dc.GetTransform(&mut t);
            dc.SetTransform(&Matrix3x2 { M31: t.M31 + m * t.M11, M32: t.M32 + m * t.M22, ..t });
            let res = f(dc);
            dc.SetTransform(&t);
            res
        });
    }

    /// A point on screen in the main grid's own coordinates (edit mode
    /// shows the grid scaled around its middle).
    pub(super) fn to_grid(&self, x: f32, y: f32) -> (f32, f32) {
        edit::unscale((x, y), grid_centre(&self.grid, &self.work), self.shown_scale())
    }

    /// A point of the main grid where it shows on screen now.
    pub(super) fn from_grid(&self, x: f32, y: f32) -> (f32, f32) {
        edit::scale((x, y), grid_centre(&self.grid, &self.work), self.shown_scale())
    }

    /// The grid's scale on screen now (mid-change: as the animation has it).
    fn shown_scale(&self) -> f32 {
        match self.arr.scale_t0 {
            Some(t0) => edit::shown_scale(self.arr.scale_from, self.arr.scale, t0.elapsed().as_secs_f32() * 1000.0, self.arr.scale_len as f32),
            None => self.arr.scale,
        }
    }

    /// Edit mode's look on (or off): the grid glides to edit::edit_scale
    /// on its plate; not animating, the plate alone (nothing changes size).
    fn edit_look(&mut self, on: bool) {
        let k = if on { edit::edit_scale(self.animate(), crate::effects::items().small) } else { 1.0 };
        let c = self.gfx.compositor.clone();
        let was = self.arr.scale;
        let shown = self.shown_scale();
        self.arr.scale = k;
        if k != was {
            // (Input follows the scale shown: shown_scale.)
            let moving = self.calm_motion() && glide(&c, &self.grid_box, "Scale", None, v3(k)).is_ok();
            if moving {
                self.arr.scale_from = if shown > 0.0 { shown } else { 1.0 };
                self.arr.scale_t0 = Some(std::time::Instant::now());
                self.arr.scale_len = glide_ms();
            } else {
                set_scale(&self.grid_box, k);
                self.arr.scale_t0 = None;
            }
        }
        if !on {
            // The plate goes as the grid glides back, from where it shows
            // now (it may still be fading in). It is left at nothing, not
            // hidden: nothing completes later that could hide one shown
            // again meanwhile.
            self.arr.page_out = fade_v(&c, &self.arr.page_v, None, 0.0, self.fx(false));
            if !self.arr.page_out {
                let _ = self.arr.page_v.SetIsVisible(false);
            }
            return;
        }
        self.draw_edit_frame();
        let _ = self.arr.page_v.SetIsVisible(true);
        // Back while it was fading out: it turns back from there.
        let from = ring_from(!std::mem::take(&mut self.arr.page_out));
        fade_v(&c, &self.arr.page_v, from, 1.0, self.fx(true));
    }

    /// The plate under the grid in edit mode, at the grid's edit scale
    /// (paint_edit_plate). Drawn when edit mode starts and again when the
    /// theme changes (no animation restarts).
    pub(super) fn draw_edit_frame(&mut self) {
        let g = self.grid;
        let (c0, k0) = (grid_centre(&self.grid, &self.work), self.arr.scale);
        let (l, t) = edit::scale((self.work.l + g.left, self.work.t + g.top), c0, k0);
        let (r, b) = edit::scale((self.work.l + g.left + g.cell_w * g.cols as f32, self.work.t + g.top + g.cell_h * g.rows as f32), c0, k0);
        let m = edit::PANEL_MARGIN;
        let rect = Rect::new(l - m, t - m, r + m, b + m);
        let v = self.arr.page_v.clone();
        let theme = &self.theme;
        self.paint_into(&v, rect, |dc| unsafe { paint_edit_plate(dc, rect.w(), rect.h(), theme) });
    }

    /// Starts edit mode on the newest app list (applied first if a newer
    /// reading is out). True: edit mode is on and the pages are as they
    /// were (a press on a slot still means the same icon).
    pub(super) fn start_edit(&mut self) -> bool {
        if self.arr.edit {
            return true;
        }
        let remade = self.reload_apps();
        if !self.can_arrange() {
            return false;
        }
        self.close_menu();
        self.arr.edit = true;
        if self.focus == Focus::Field {
            self.focus = Focus::Grid;
            if !self.sel.is_some_and(|s| self.valid_sel(s)) {
                self.sel = self.first_on_page();
            }
        }
        self.edit_look(true);
        self.draw_done();
        self.draw_field();
        self.update_sel();
        self.announce(t("launchpad.edit.on").to_string());
        !remade
    }

    /// Edit mode ends: a folder open closes, a drag goes back, the pages
    /// are saved if they changed.
    pub(super) fn end_edit(&mut self) {
        if !self.arr.edit {
            return;
        }
        if self.arr.folder.is_some() {
            self.close_folder();
        }
        self.arr_cancel_drag();
        self.arr.lifted = None;
        self.arr.edit = false;
        // Nothing stays picked (organize.rs).
        self.clear_picks();
        self.edit_look(false);
        self.draw_done();
        if let Some(l) = self.layout_to_save() {
            self.arr.saved = l.clone();
            self.arr.dirty = false;
            self.deferred.push(Deferred::SaveLayout(l));
        }
        self.draw_field();
        self.update_sel();
        self.announce(t("launchpad.edit.off").to_string());
    }

    /// The pages to save if they changed (a drag under way counts as put
    /// back).
    pub(super) fn layout_to_save(&mut self) -> Option<Vec<Vec<LpItem>>> {
        if self.arr.drag.is_some() || self.arr.folder.as_ref().is_some_and(|f| f.drag.is_some()) {
            self.clear_feedback();
        }
        if let Some(d) = self.arr.drag.take() {
            self.layout = d.before;
        }
        if let Some(fd) = self.arr.folder.as_mut().and_then(|f| f.drag.take()) {
            self.layout = fd.before;
        }
        if self.arr.rename.is_some() {
            self.commit_rename();
        }
        if self.arr.dirty && !self.apps_complete {
            // (Cannot happen: arranging needs a complete list, and an
            // incomplete one is never taken while arranged. Checked anyway.)
            crate::log!("launchpad: not saving an arrangement made over an incomplete app list");
            return None;
        }
        self.arr.dirty.then(|| layout_store::to_config(&self.layout))
    }

    /// Typing goes to the search field: edit mode and the open folder end.
    pub(super) fn arr_leave_for_typing(&mut self) {
        if self.arr.folder.is_some() {
            self.close_folder();
        }
        self.end_edit();
    }

    /// The motion settings changed: edit mode's look follows (no scale
    /// without motion).
    pub(super) fn arr_motion_changed(&mut self) {
        if self.arr.edit {
            self.arr.scale_t0 = None;
            self.arr.scale = 0.0;
            self.edit_look(true);
        }
    }

    /// The feedback of a drag goes: the copy, the target, the lit edge.
    fn clear_feedback(&mut self) {
        self.clear_hold();
        self.stop_edge();
        self.stop_settle();
        self.arr.drag_gen += 1;
        let _ = self.arr.ghost_v.SetIsVisible(false);
        // (The border is drawn into the panel: drawn again unlit.)
        if std::mem::take(&mut self.arr.border_lit) && self.arr.folder.is_some() {
            self.draw_panel();
        }
    }

    /// The app list is read again: a drag goes back, the folder closes.
    pub(super) fn arr_reset_for_reload(&mut self) {
        self.arr_cancel_long_press();
        if let Some(d) = self.arr.drag.take() {
            self.layout = d.before;
            self.drag_gone(None);
            self.clear_feedback();
            unsafe {
                let _ = ReleaseCapture();
            }
        }
        if self.arr.folder.is_some() {
            self.close_folder();
        }
        self.arr.lifted = None;
    }

    /// Edit mode's Done (top right; top left right to left): a capsule a
    /// control tall, a raised thing (paint_done). Its fill is a brush of
    /// its own under the picture, so it can fade between its states; the
    /// keyboard's ring shows while a screen reader's focus is on it.
    pub(super) fn draw_done(&mut self) {
        if !self.arr.edit {
            let _ = self.arr.done_v.SetIsVisible(false);
            let _ = self.arr.done_fill.SetIsVisible(false);
            self.arr.done_rect = Rect::new(0.0, 0.0, 0.0, 0.0);
            // (Let go on it, it went pressed: at rest the next time.)
            self.arr.done_state = State::Rest;
            set_scale(&self.arr.done_fill, 1.0);
            set_scale(&self.arr.done_v, 1.0);
            return;
        }
        let Ok((_, tw, _)) = self.gfx.text_style(t("launchpad.edit.done"), &self.theme.font.family, ramp::BODY_STRONG, 1.0) else { return };
        let (w, h) = done_size(tw);
        let r = done_place(&self.work, w, self.rtl);
        self.arr.done_rect = r;
        let state = self.arr.done_state;
        // The fill, where the picture lands (whole device pixels).
        let fill_v = &self.arr.done_fill;
        let _ = fill_v.SetOffset(Vector3 { X: self.snap(r.l), Y: self.snap(r.t), Z: 0.0 });
        let _ = fill_v.SetSize(Vector2 { X: w, Y: h });
        let _ = self.arr.done_geom.SetSize(Vector2 { X: w, Y: h });
        let rad = crate::theme::capsule(h);
        let _ = self.arr.done_geom.SetCornerRadius(Vector2 { X: rad, Y: rad });
        color_to(&self.gfx.compositor, &self.arr.done_brush, None, color(rgba(self.theme.fill_of(Role::Raised, state))), (0, crate::motion::LINEAR));
        let _ = fill_v.SetIsVisible(true);
        self.done_picture();
    }

    /// Done's picture (paint_done) in its state now, where draw_done put
    /// it: its fill (a visual of its own) is left as it is, fading.
    fn done_picture(&mut self) {
        let Ok((l, tw, th)) = self.gfx.text_style(t("launchpad.edit.done"), &self.theme.font.family, ramp::BODY_STRONG, 1.0) else { return };
        let (w, h) = done_size(tw);
        let r = self.arr.done_rect;
        let state = self.arr.done_state;
        let focus = self.ax_focus.as_deref() == Some("done") && self.reader_ring;
        self.arr.done_ring = focus;
        let v = self.arr.done_v.clone();
        let theme = &self.theme;
        self.paint_with_room(&v, r, DONE_M, |dc| unsafe { paint_done(dc, w, h, (&l, tw, th), theme, state, false, focus) });
    }

    /// Done's state follows the pointer: its fill fades (90 on, 120 off;
    /// pressed at once, 6.3) and, pressed, it shrinks to the press scale
    /// on SNAP (not when Launchpad does not animate).
    fn done_to(&mut self, state: State) {
        let was = std::mem::replace(&mut self.arr.done_state, state);
        if was == state || !self.arr.edit {
            return;
        }
        let c = self.gfx.compositor.clone();
        let plan = if state == State::Pressed { (0, crate::motion::LINEAR) } else { self.fx(state == State::Hover) };
        color_to(&c, &self.arr.done_brush, None, color(rgba(self.theme.fill_of(Role::Raised, state))), plan);
        let (w, h) = (self.arr.done_rect.w(), self.arr.done_rect.h());
        if self.animate() && (state == State::Pressed || was == State::Pressed) {
            let k = if state == State::Pressed { crate::motion::press_scale(w, h) } else { 1.0 };
            let _ = crate::motion::scale_to(&c, &self.arr.done_fill, (w, h), k);
            let _ = crate::motion::scale_to(&c, &self.arr.done_v, (w + DONE_M * 2.0, h + DONE_M * 2.0), k);
        }
        // (High contrast marks the state in the picture: its outline, its
        // ink; Flat v2 its card light, gone pressed.)
        if self.hc().is_some() {
            self.draw_done();
        } else if self.theme.card_fx(State::Rest).is_some() {
            self.done_picture();
        }
    }

    /// The pointer at (x, y) in edit mode: Done under it is lit; pressed,
    /// it stays pressed while the pointer is on it.
    fn done_point(&mut self, x: f32, y: f32) {
        if !self.arr.edit {
            return;
        }
        let over = self.arr.folder.is_none() && self.arr.drag.is_none() && self.org.band.is_none() && self.arr.done_rect.contains(x, y);
        let held = self.press.as_ref().is_some_and(|p| p.hit == Hit::Done);
        let pressed_elsewhere = self.press.is_some() && !held;
        self.done_to(match (over, held) {
            (true, true) => State::Pressed,
            (true, false) if !pressed_elsewhere => State::Hover,
            _ => State::Rest,
        });
    }

    /// A screen reader's focus may have come to Done or left it: its ring
    /// follows (drawn again only when it changed).
    pub(super) fn done_focus_changed(&mut self) {
        let ring = self.ax_focus.as_deref() == Some("done") && self.reader_ring;
        if self.arr.edit && ring != self.arr.done_ring {
            self.draw_done();
        }
    }

    // ---- hits and the long press ------------------------------------------------

    /// The open folder's parts (it covers everything else), Done.
    pub(super) fn arr_hit(&self, x: f32, y: f32) -> Option<Hit> {
        if let Some(f) = &self.arr.folder {
            let panel = folders::panel_rect(&f.pg).offset(self.work.l, self.work.t);
            if !panel.contains(x, y) {
                return Some(Hit::FolderOutside);
            }
            if y < panel.t + folders::PANEL_TITLE + folders::PANEL_PAD / 2.0 {
                return Some(Hit::FolderName);
            }
            if let Some(slot) = f.pg.slot_at(x - self.work.l, y - self.work.t, self.rtl) {
                let i = f.scroll * f.pg.cols + slot;
                if i < self.folder_apps().len() {
                    return Some(Hit::FolderApp(i));
                }
            }
            return Some(Hit::FolderPanel);
        }
        (self.arr.edit && self.arr.done_rect.contains(x, y)).then_some(Hit::Done)
    }

    pub(super) fn arr_cancel_long_press(&mut self) {
        if std::mem::take(&mut self.arr.long_press) {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), LONG_PRESS_TIMER);
            }
        }
    }

    fn arm_long_press(&mut self) {
        self.arr.long_press = unsafe { SetTimer(Some(self.hwnd), LONG_PRESS_TIMER, edit::LONG_PRESS_MS, None) } != 0;
    }

    /// A press: in edit mode and on the open folder all of it is ours; on an
    /// app it may become edit mode (a long press). True: handled.
    pub(super) fn arr_down(&mut self, x: f32, y: f32, hit: Hit) -> bool {
        let capture = |p: &mut Pad, hit: Hit| {
            p.press = Some(Press { x, y, hit });
            unsafe {
                SetCapture(p.hwnd);
            }
        };
        if self.arr.folder.is_some() {
            match hit {
                Hit::FolderApp(i) => {
                    if self.arr.rename.is_some() {
                        self.commit_rename();
                    }
                    if let Some(f) = self.arr.folder.as_mut() {
                        f.sel = Some(i);
                    }
                    capture(self, hit);
                    // (A Library category is read only: no edit mode from it.)
                    let category = self.arr.folder.as_ref().is_some_and(|f| f.cat.is_some());
                    if !self.arr.edit && !category && self.can_arrange() {
                        self.arm_long_press();
                    }
                }
                Hit::FolderName if self.arr.edit => {
                    if self.arr.rename.is_none() {
                        self.start_rename(false);
                    }
                }
                Hit::FolderOutside => {
                    if self.arr.rename.is_some() {
                        self.commit_rename();
                    }
                    self.close_folder();
                }
                _ => {
                    if self.arr.rename.is_some() {
                        self.commit_rename();
                    }
                }
            }
            return true;
        }
        if self.arr.edit {
            capture(self, hit);
            self.done_point(x, y);
            return true;
        }
        if matches!(hit, Hit::App(_)) && self.can_arrange() {
            self.arm_long_press();
        }
        false
    }

    /// A timer of edit mode fired.
    pub(super) fn arr_timer(&mut self, id: usize) {
        match id {
            LONG_PRESS_TIMER => {
                if !std::mem::take(&mut self.arr.long_press) || self.drag.is_some() {
                    return;
                }
                let Some((px, py, hit)) = self.press.as_ref().map(|p| (p.x, p.y, p.hit)) else { return };
                match hit {
                    Hit::App(slot) => {
                        self.unpress();
                        if self.start_edit() {
                            self.lift(slot, (px, py));
                        }
                    }
                    Hit::FolderApp(i) => {
                        if self.start_edit() && self.arr.folder.is_some() {
                            self.draw_panel();
                            self.folder_lift(i, (px, py));
                        }
                    }
                    _ => {}
                }
            }
            // The plate is full: dropping now makes the folder.
            HOLD_TIMER => {
                if self.arr.drag.is_some() && self.arr.hold.is_some() {
                    self.arr.hold_ready = true;
                    self.paint_plate(true);
                }
            }
            EDGE_TIMER => self.edge_flip(),
            SETTLE_TIMER => self.settled(),
            _ => {}
        }
    }

    /// The pointer moved. True: ours (a drag, or a press about to lift).
    pub(super) fn arr_move(&mut self, x: f32, y: f32) -> bool {
        self.done_point(x, y);
        if self.arr.folder.as_ref().is_some_and(|f| f.drag.is_some()) {
            self.folder_drag_move(x, y);
            return true;
        }
        if self.arr.drag.is_some() {
            self.drag_move(x, y);
            return true;
        }
        if self.org.band.is_some() {
            self.band_move(x, y);
            return true;
        }
        let Some((px, py, hit)) = self.press.as_ref().map(|p| (p.x, p.y, p.hit)) else { return false };
        let far = dist((x, y), (px, py)) > LIFT_START;
        if far {
            self.arr_cancel_long_press();
        }
        if !(self.arr.edit && far) {
            return false;
        }
        match hit {
            Hit::App(slot) if !self.searching => {
                self.lift(slot, (px, py));
                self.drag_move(x, y);
                true
            }
            Hit::FolderApp(i) => {
                self.folder_lift(i, (px, py));
                self.folder_drag_move(x, y);
                true
            }
            // Over empty space: a band picks what it touches (organize.rs);
            // a finger or pen still swipes the pages.
            Hit::Empty if !self.searching && !self.on_lib() && self.arr.folder.is_none() && !super::organize::from_touch() => {
                self.band_start(px, py);
                self.band_move(x, y);
                true
            }
            _ => false,
        }
    }

    /// A release. True: ours.
    pub(super) fn arr_up(&mut self, x: f32, y: f32) -> bool {
        if self.arr.folder.as_ref().is_some_and(|f| f.drag.is_some()) {
            self.press = None;
            unsafe {
                let _ = ReleaseCapture();
            }
            self.folder_drop();
            return true;
        }
        if self.arr.drag.is_some() {
            self.press = None;
            unsafe {
                let _ = ReleaseCapture();
            }
            self.drop_icon();
            return true;
        }
        if self.org.band.is_some() {
            self.press = None;
            self.band_end();
            unsafe {
                let _ = ReleaseCapture();
            }
            return true;
        }
        if self.arr.folder.is_some() {
            let press = self.press.take();
            unsafe {
                let _ = ReleaseCapture();
            }
            let hit = self.hit(x, y);
            if let (Some(p), Hit::FolderApp(i)) = (press, hit) {
                if p.hit == hit && !self.arr.edit {
                    self.launch_from_folder(i);
                }
            }
            return true;
        }
        if !self.arr.edit || self.drag.is_some() {
            return false;
        }
        let hit = self.hit(x, y);
        let Some(press_hit) = self.press.as_ref().map(|p| p.hit) else { return false };
        if press_hit != hit {
            self.press = None;
            unsafe {
                let _ = ReleaseCapture();
            }
            self.done_point(x, y);
            return true;
        }
        match hit {
            Hit::App(slot) => {
                self.press = None;
                unsafe {
                    let _ = ReleaseCapture();
                }
                // Ctrl / Shift: picked with others (organize.rs). Else a
                // tap opens a folder; an app is only selected; nothing
                // stays picked.
                if let Some(i) = self.index_of_slot(slot) {
                    let (ctrl, shift) = (key_down(VK_CONTROL), key_down(VK_SHIFT));
                    if ctrl || shift {
                        self.pick_click(i, ctrl, shift);
                        return true;
                    }
                    self.plain_click(i);
                    if matches!(self.item_at(i), Some(LpItem::Folder { .. })) {
                        self.open_folder(i);
                    } else {
                        self.sel = Some(i);
                        self.focus = Focus::Grid;
                        self.update_sel();
                        self.publish();
                    }
                }
                true
            }
            Hit::Done => {
                self.press = None;
                unsafe {
                    let _ = ReleaseCapture();
                }
                self.end_edit();
                true
            }
            Hit::Empty => {
                self.press = None;
                unsafe {
                    let _ = ReleaseCapture();
                }
                // With icons picked it only lets them go; else Done.
                if self.org.picks.is_empty() {
                    self.end_edit();
                } else {
                    self.clear_picks();
                    self.publish();
                }
                true
            }
            // The field: edit mode ends and it takes the focus (as usual).
            Hit::Field => {
                self.end_edit();
                false
            }
            _ => false,
        }
    }

    // ---- dragging an icon on the pages ---------------------------------------------

    /// Picks up the item in slot `slot` of the page on screen, the pointer
    /// at `at` (it is taken out of the pages; the others will make room).
    fn lift(&mut self, slot: usize, at: (f32, f32)) {
        // (Search results are by name: slots there are not places.)
        if self.searching || !self.arr.edit {
            return;
        }
        let Some(i) = self.index_of_slot(slot) else { return };
        let pos = self.pos_of(i);
        let before = self.layout.clone();
        // A picked icon takes the other picked ones with it; another one
        // goes alone and nothing stays picked (organize.rs).
        let key = self.item_at(i).map(super::super::picks::key_of);
        let many = self.org.picks.len() >= 2 && key.as_ref().is_some_and(|k| self.org.picks.contains(k));
        if !many {
            self.clear_picks();
        }
        let (item, others, lead_at, gap) = if many {
            let ps = self.org.picks.positions(&self.layout);
            let lead_at = ps.iter().position(|p| *p == pos).unwrap_or(0);
            // Where it was once the ones before it on its page are out.
            let gap = pos.slot - ps.iter().filter(|p| p.page == pos.page && p.slot < pos.slot).count();
            let mut all = super::super::picks::take_many(&mut self.layout, &ps);
            if lead_at >= all.len() {
                self.layout = before;
                return;
            }
            let item = all.remove(lead_at);
            (item, all, lead_at, gap)
        } else {
            let Some(item) = edit::take(&mut self.layout, pos) else { return };
            (item, Vec::new(), 0, pos.slot)
        };
        let Some(tile) = self.item_tile(&item) else {
            self.layout = before;
            return;
        };
        // Where the icon shows on screen (the grid scaled in edit mode).
        let ir = self.icon_rect(&self.grid, slot);
        let centre = self.from_grid(ir.cx(), ir.cy());
        let brush = self.tile_brush(&tile).or_else(|| self.placeholder());
        let tl = self.show_dragged(brush, centre, ir.w(), self.shown_scale());
        self.arr.lifted = None;
        let n = others.len() + 1;
        self.arr.drag = Some(IconDrag { item, others, lead_at, key: tile.key, page: pos.page, gap, grab: (at.0 - tl.0, at.1 - tl.1), start: at, moved: false, before });
        self.arr.drag_gen += 1;
        self.press = None;
        unsafe {
            SetCapture(self.hwnd);
        }
        self.update_sel();
        if n > 1 {
            // (The others left other places, maybe other pages.)
            let shown: Vec<usize> = self.pages.keys().copied().collect();
            for p in shown {
                self.bind_page(p);
            }
        } else {
            self.bind_page(pos.page);
        }
        self.place_ghost();
        if n > 1 {
            self.announce(tf("launchpad.edit.lifted_n", &[("count", &n.to_string())]));
        } else {
            self.announce(tf("launchpad.edit.lifted", &[("name", &tile.name)]));
        }
    }

    /// The dragged visual: `brush`, `e` DIP across (the icon it was lifted
    /// from as drawn), centred on `centre` (screen DIP). Picked up (2.6):
    /// from scale `from` (as it showed in its cell) it glides LIFT_SCALE
    /// bigger and its shadows (elevation E3, shaped like the icon) come up
    /// with it; not animating its size stays and the shadows fade in. Its
    /// top-left corner (the pointer keeps its offset from it).
    fn show_dragged(&mut self, brush: Option<CompositionSurfaceBrush>, centre: (f32, f32), e: f32, from: f32) -> (f32, f32) {
        self.arr.drag_e = e;
        let c = self.gfx.compositor.clone();
        let (v, icon, shade) = (&self.arr.drag_v, &self.arr.drag_icon, &self.arr.drag_shade);
        let size = Vector2 { X: e, Y: e };
        // (A drop's way home may still be playing: it ends here.)
        let _ = v.StopAnimation(&HSTRING::from("Offset"));
        if let Some(b) = &brush {
            let _ = icon.SetBrush(b);
        }
        let _ = icon.SetSize(size);
        let _ = icon.SetIsVisible(true);
        // The shadows a thing picked up casts: one host a layer, each the
        // icon's own shape (none in high contrast or with "Shadows" off).
        let _ = shade.Children().and_then(|ch| ch.RemoveAll());
        let layers = if crate::effects::items().shadows { self.theme.elevation(Elevation::E3) } else { Vec::new() };
        for (x, y, blur, col) in layers {
            let Some(mask) = &brush else { break };
            let _ = (|| -> Result<()> {
                let host = c.CreateSpriteVisual()?;
                host.SetSize(size)?;
                let shadow = c.CreateDropShadow()?;
                shadow.SetBlurRadius(crate::theme::composition_blur(blur))?;
                shadow.SetOffset(Vector3 { X: x, Y: y, Z: 0.0 })?;
                shadow.SetColor(Color { A: col.a, R: col.r, G: col.g, B: col.b })?;
                shadow.SetMask(mask)?;
                host.SetShadow(&shadow)?;
                shade.Children()?.InsertAtTop(&host)
            })();
        }
        let _ = v.SetSize(size);
        let _ = v.SetCenterPoint(Vector3 { X: e / 2.0, Y: e / 2.0, Z: 0.0 });
        let b = super::dragged_box(centre, e);
        let tl = (b.l, b.t);
        let _ = v.SetOffset(Vector3 { X: tl.0, Y: tl.1, Z: 0.0 });
        let lifts = self.calm_motion() && glide(&c, v, "Scale", Some(v3(from)), v3(from * edit::LIFT_SCALE)).is_ok() && glide_opacity(&c, shade, Some(0.0), 1.0).is_ok();
        if !lifts {
            // (Nothing changes size: 12.2.)
            set_scale(v, from);
            fade_v(&c, shade, Some(0.0), 1.0, self.fx(true));
        }
        let _ = v.SetIsVisible(true);
        tl
    }

    /// The dragged visual goes. Dropped (`home`: its cell's top-left corner
    /// on screen and the scale it shows at there), its shadows glide home
    /// with the cell's icon and go out on the way (6.2: the shadow comes
    /// back with it); else they fade out where they are.
    fn drag_gone(&mut self, home: Option<(Vector3, f32)>) {
        let c = self.gfx.compositor.clone();
        let (v, icon, shade) = (&self.arr.drag_v, &self.arr.drag_icon, &self.arr.drag_shade);
        if !v.IsVisible().unwrap_or(false) {
            return;
        }
        let _ = icon.SetIsVisible(false);
        let glides = match home.filter(|_| self.calm_motion()) {
            Some((to, k)) => glide(&c, v, "Offset", None, to).is_ok() && glide(&c, v, "Scale", None, v3(k)).is_ok() && glide_opacity(&c, shade, None, 0.0).is_ok(),
            None => false,
        };
        if !glides && !fade_v(&c, shade, None, 0.0, self.fx(false)) {
            let _ = v.SetIsVisible(false);
        }
    }

    /// The slot whose cell is under (x, y) on grid `g` (its own
    /// coordinates), and where on it relative to its icon.
    fn cell_under(&self, g: &Grid, n: usize, x: f32, y: f32) -> Option<(usize, edit::Zone)> {
        (0..n).find_map(|s| {
            let c = g.cell(s, self.rtl).offset(self.work.l, self.work.t);
            if !c.contains(x, y) {
                return None;
            }
            let i = g.icon(s, self.rtl).offset(self.work.l, self.work.t);
            Some((s, edit::zone(x - i.cx(), y - i.cy(), g.icon_edge, self.rtl)))
        })
    }

    fn drag_move(&mut self, x: f32, y: f32) {
        let (pp, rtl, w) = (self.per_page(), self.rtl, self.w);
        let Some(d) = self.arr.drag.as_mut() else { return };
        if !d.moved && dist((x, y), d.start) > 2.0 {
            d.moved = true;
        }
        // 1:1 with the pointer: set, never animated.
        let at = Vector3 { X: x - d.grab.0, Y: y - d.grab.1, Z: 0.0 };
        let _ = self.arr.drag_v.SetOffset(at);
        let (page, gap) = (d.page, d.gap);
        let folder_ok = d.apps_only();
        // A side edge (with a page that way): lit at once, the page turns
        // after EDGE_MS.
        let edge = edit::edge(x, w, rtl).filter(|&next| if next { page + 1 < self.pages_n() } else { page > 0 });
        match edge {
            Some(next) if self.arr.edge != Some(next) => {
                self.stop_edge();
                self.arr.edge = Some(next);
                self.show_edge(next);
                unsafe {
                    SetTimer(Some(self.hwnd), EDGE_TIMER, edit::EDGE_MS, None);
                }
            }
            Some(_) => {}
            None => self.stop_edge(),
        }
        let n = self.page_len(page);
        let (gx, gy) = self.to_grid(x, y);
        let shown = self.cell_under(&self.grid, pp, gx, gy);
        match edit::aim(shown, gap, n) {
            Aim::Onto(i) if folder_ok => {
                // The folder intent: no reorder waits meanwhile.
                self.stop_settle();
                if let edit::Hold::Start(i) = edit::hold(self.arr.hold, Some(i)) {
                    self.start_hold(i);
                }
            }
            // (A folder onto an icon: nothing; the gap stays.)
            Aim::Onto(_) => {
                self.clear_hold();
                self.stop_settle();
            }
            Aim::Gap(g) => {
                self.clear_hold();
                self.aim_gap(gap, g);
            }
        }
    }

    /// The reorder intent aims at gap `g` (the gap is at `gap`): it moves
    /// there once the pointer stays SETTLE_MS.
    fn aim_gap(&mut self, gap: usize, g: usize) {
        match edit::settle(gap, self.arr.pending, g) {
            edit::Settle::Keep => {}
            edit::Settle::Clear => {
                self.stop_settle();
                self.place_ghost();
            }
            edit::Settle::Arm(g) => {
                self.arr.pending = Some(g);
                self.arr.pending_gen = self.arr.drag_gen;
                unsafe {
                    SetTimer(Some(self.hwnd), SETTLE_TIMER, edit::SETTLE_MS, None);
                }
                // The destination shows at once; the others move after.
                self.place_ghost();
            }
        }
    }

    fn stop_settle(&mut self) {
        if self.arr.pending.take().is_some() {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), SETTLE_TIMER);
            }
        }
    }

    /// The pointer stayed with the same place: the gap moves there, the
    /// others slide out of the way (REFLOW_MS).
    fn settled(&mut self) {
        let Some(g) = self.arr.pending.take() else { return };
        // (A wait from a drag that has ended since: nothing.)
        if self.arr.pending_gen != self.arr.drag_gen {
            return;
        }
        if let Some(d) = self.arr.folder.as_mut().and_then(|f| f.drag.as_mut()) {
            d.gap = g;
            self.bind_folder();
        } else if let Some(d) = self.arr.drag.as_mut() {
            d.gap = g;
            let page = d.page;
            self.bind_page(page);
        } else {
            return;
        }
        self.place_ghost();
    }

    /// The faint copy of the dragged icon where it would go (in the
    /// container of the page or the folder it is over, so it scales and
    /// moves with it).
    fn place_ghost(&mut self) {
        let spot = if let Some(f) = self.arr.folder.as_ref().filter(|f| f.drag.is_some()) {
            let d = f.drag.as_ref().expect("checked");
            // Where it would go: the place waited for, else the gap.
            let g = self.arr.pending.unwrap_or(d.gap);
            let slot = g.checked_sub(f.scroll * f.pg.cols).filter(|&s| s < f.pg.cols * f.pg.rows);
            let container = self.arr.folder_view.as_ref().map(|v| v.container.clone());
            slot.zip(container).map(|(s, c)| (c, self.icon_rect(&f.pg, s)))
        } else if let Some(d) = &self.arr.drag {
            let container = self.pages.get(&d.page).map(|v| v.container.clone());
            let g = self.arr.pending.unwrap_or(d.gap);
            (g < self.per_page()).then_some(()).and(container).map(|c| (c, self.icon_rect(&self.grid, g)))
        } else {
            None
        };
        let v = self.arr.ghost_v.clone();
        let Some((container, r)) = spot else {
            let _ = v.SetIsVisible(false);
            return;
        };
        let to = Vector3 { X: r.l, Y: r.t, Z: 0.0 };
        let same = v.Parent().ok().is_some_and(|p| p == container) && v.IsVisible().unwrap_or(false);
        if !same {
            if let Ok(p) = v.Parent() {
                let _ = p.Children().and_then(|ch| ch.Remove(&v));
            }
            if container.Children().and_then(|ch| ch.InsertAtBottom(&v)).is_err() {
                return;
            }
        }
        if let Ok(b) = self.arr.drag_icon.Brush() {
            let _ = v.SetBrush(&b);
        }
        // (The icon's own square there: smaller icons, a smaller ghost.)
        let _ = v.SetSize(Vector2 { X: r.w(), Y: r.h() });
        let _ = v.SetOpacity(edit::GHOST_OPACITY);
        let from = v.Offset().unwrap_or(to);
        let _ = v.SetOffset(to);
        // (To another place: it glides there, or comes in there in place.)
        if same && (from.X != to.X || from.Y != to.Y) {
            let _ = slide(&self.gfx.compositor, &v, from, to, self.calm_motion());
        }
        let _ = v.SetIsVisible(true);
    }

    /// The folder intent on item `i` of the dragged icon's page: shown at
    /// once (the target grows on SNAP, a plate fades in behind it and grows
    /// over FOLDER_DWELL_MS); ready when it is full (HOLD_TIMER). Not
    /// animating, nothing grows: the plate fades in at its full size.
    fn start_hold(&mut self, i: usize) {
        self.clear_hold();
        let Some((page, gap)) = self.arr.drag.as_ref().map(|d| (d.page, d.gap)) else { return };
        let slot = edit::shown_slot(i, Some(gap));
        let (Some(view), true) = (self.pages.get(&page), slot < self.per_page()) else { return };
        let (container, target) = (view.container.clone(), view.cells[slot].0.clone());
        self.arr.hold = Some(i);
        self.arr.hold_ready = false;
        unsafe {
            SetTimer(Some(self.hwnd), HOLD_TIMER, edit::FOLDER_DWELL_MS, None);
        }
        let c = self.gfx.compositor.clone();
        let motion = self.calm_motion();
        if motion {
            let e = self.icon_rect(&self.grid, slot).w();
            let _ = target.StopAnimation(&HSTRING::from("Scale"));
            let _ = crate::motion::scale_to(&c, &target, (e, e), edit::TARGET_SCALE);
        }
        self.arr.target = Some(target);
        // The plate (in the page's container, under the icons).
        let ir = self.icon_rect(&self.grid, slot);
        let side = ir.w() * edit::PLATE;
        let r = Rect::new(ir.cx() - side / 2.0, ir.cy() - side / 2.0, ir.cx() + side / 2.0, ir.cy() + side / 2.0);
        let v = self.arr.plate_v.clone();
        if let Ok(p) = v.Parent() {
            let _ = p.Children().and_then(|ch| ch.Remove(&v));
        }
        if container.Children().and_then(|ch| ch.InsertAtBottom(&v)).is_err() {
            return;
        }
        self.arr.plate_rect = r;
        self.paint_plate(false);
        let size = v.Size().unwrap_or(Vector2 { X: side, Y: side });
        let _ = v.SetCenterPoint(Vector3 { X: size.X / 2.0, Y: size.Y / 2.0, Z: 0.0 });
        fade_v(&c, &v, Some(0.0), 1.0, self.fx(true));
        // The growth shows the wait: straight, on real time like the timer
        // (not at the animation speed), full exactly when dropping makes
        // the folder.
        let grows = motion
            && (|| -> Result<()> {
                let a = c.CreateVector3KeyFrameAnimation()?;
                a.InsertKeyFrame(0.0, v3(edit::PLATE_FROM))?;
                a.InsertKeyFrameWithEasingFunction(1.0, v3(1.0), &c.CreateLinearEasingFunction()?)?;
                a.SetDuration(real(edit::FOLDER_DWELL_MS as i64))?;
                v.StartAnimation(&HSTRING::from("Scale"), &a)
            })()
            .is_ok();
        if !grows {
            set_scale(&v, 1.0);
        }
        if let Some(name) = self.layout.get(page).and_then(|p| p.get(i)).and_then(|it| self.item_tile(it)).map(|t| t.name) {
            self.announce(tf("launchpad.edit.into_folder", &[("name", &name)]));
        }
    }

    /// The folder plate (paint_drop_plate): waiting, or ready (drop now
    /// makes the folder: the stronger ink; the only sign of it when
    /// nothing grows).
    fn paint_plate(&mut self, ready: bool) {
        let r = self.arr.plate_rect;
        let v = self.arr.plate_v.clone();
        let theme = &self.theme;
        self.paint_into(&v, r, |dc| unsafe { paint_drop_plate(dc, r.w(), theme, ready) });
    }

    /// The folder intent ends (out of the middle): the target back to its
    /// size (SNAP), the plate out.
    fn clear_hold(&mut self) {
        let held = self.arr.hold.take().is_some();
        if held {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), HOLD_TIMER);
            }
        }
        self.arr.hold_ready = false;
        let c = self.gfx.compositor.clone();
        if let Some(t_) = self.arr.target.take() {
            let size = t_.Size().unwrap_or_default();
            if !(self.calm_motion() && crate::motion::scale_to(&c, &t_, (size.X, size.Y), 1.0).is_ok()) {
                set_scale(&t_, 1.0);
            }
        }
        // (Left at nothing, not hidden: the next target shows it again.)
        if !(held && fade_v(&c, &self.arr.plate_v, None, 0.0, self.fx(false))) {
            let _ = self.arr.plate_v.SetIsVisible(false);
        }
    }

    /// The edge a dragged icon rests at lights (`next`: the edge that goes
    /// on, right; left right to left).
    fn show_edge(&mut self, next: bool) {
        let right = next != self.rtl;
        let x = if right { self.w - edit::EDGE } else { 0.0 };
        let r = Rect::new(x, self.work.t, x + edit::EDGE, self.work.b);
        let v = self.arr.edge_v.clone();
        let theme = &self.theme;
        self.paint_into(&v, r, |dc| unsafe { paint_edge(dc, r.w(), r.h(), theme) });
        fade_v(&self.gfx.compositor, &v, Some(0.0), 1.0, self.fx(true));
    }

    fn stop_edge(&mut self) {
        if self.arr.edge.take().is_none() {
            return;
        }
        unsafe {
            let _ = KillTimer(Some(self.hwnd), EDGE_TIMER);
        }
        // (Left at nothing, not hidden: lit again, it fades back in.)
        if !fade_v(&self.gfx.compositor, &self.arr.edge_v, None, 0.0, self.fx(false)) {
            let _ = self.arr.edge_v.SetIsVisible(false);
        }
    }

    /// Resting at a side edge: the next / previous page (the empty page
    /// after the last one too). Repeats every EDGE_MS while it stays there.
    fn edge_flip(&mut self) {
        let (Some(next), Some(page)) = (self.arr.edge, self.arr.drag.as_ref().map(|d| d.page)) else {
            self.stop_edge();
            return;
        };
        let to = if next { page + 1 } else { page.wrapping_sub(1) };
        if to >= self.pages_n() {
            self.stop_edge();
            return;
        }
        self.clear_hold();
        self.stop_settle();
        let len = self.page_len(to);
        if let Some(d) = self.arr.drag.as_mut() {
            d.page = to;
            d.gap = len;
        }
        // The page left closes its gap; the new one opens one at its end.
        self.bind_page(page);
        self.go_page(to, true);
        self.place_ghost();
        // No page further that way: the edge goes dark.
        let more = if next { to + 1 < self.pages_n() } else { to > 0 };
        if !more {
            self.stop_edge();
        }
        self.announce(tf("launchpad.page", &[("n", &(to + 1).to_string()), ("count", &self.pages_n().to_string())]));
    }

    /// Where the dragged visual's icon is in the grid's own coordinates
    /// (its top-left corner at its own size): where a dropped icon slides
    /// home from.
    fn dropped_at(&self, in_grid: bool) -> Vector3 {
        let at = self.arr.drag_v.Offset().unwrap_or_default();
        if !in_grid {
            return at;
        }
        let e = self.arr.drag_e;
        let (cx, cy) = self.to_grid(at.X + e / 2.0, at.Y + e / 2.0);
        Vector3 { X: cx - e / 2.0, Y: cy - e / 2.0, Z: 0.0 }
    }

    /// The dragged icon is let go: onto the icon whose plate is full (a
    /// folder), else where the pointer says (the place it waits for counts).
    /// Empty pages go.
    fn drop_icon(&mut self) {
        let pending = self.arr.pending;
        let hold = self.arr.hold.filter(|_| self.arr.hold_ready);
        let at = self.dropped_at(true);
        self.clear_feedback();
        let Some(d) = self.arr.drag.take() else { return };
        if !d.moved {
            // Lifted and let go where it was: nothing changes.
            self.drag_gone(None);
            self.layout = d.before;
            let shown: Vec<usize> = self.pages.keys().copied().collect();
            for p in shown {
                self.bind_page(p);
            }
            self.update_sel();
            return;
        }
        let gap = pending.unwrap_or(d.gap);
        let pp = self.per_page();
        let name = t("launchpad.folder.default");
        let mut new_folder = false;
        let (page, key, many) = (d.page, d.key.clone(), !d.others.is_empty());
        let landed = if many {
            // Several: together, in their order (organize.rs).
            use super::super::picks;
            match hold {
                Some(i) => {
                    let was_app = matches!(self.layout.get(page).and_then(|p| p.get(i)), Some(LpItem::App(_)));
                    match picks::combine_many(&mut self.layout, Pos::new(page, i), d.all(), name) {
                        Ok(()) => {
                            new_folder = was_app;
                            Pos::new(page, i)
                        }
                        Err(items) => picks::put_many(&mut self.layout, Pos::new(page, gap), items, pp),
                    }
                }
                None => picks::put_many(&mut self.layout, Pos::new(page, gap), d.all(), pp),
            }
        } else {
            match hold {
                Some(i) => {
                    let was_app = matches!(self.layout.get(page).and_then(|p| p.get(i)), Some(LpItem::App(_)));
                    match folders::combine(&mut self.layout, Pos::new(page, i), d.item, name) {
                        Ok(()) => {
                            new_folder = was_app;
                            Pos::new(page, i)
                        }
                        Err(item) => edit::put(&mut self.layout, Pos::new(page, gap), item, pp),
                    }
                }
                None => edit::put(&mut self.layout, Pos::new(page, gap), d.item, pp),
            }
        };
        let landed = edit::tidy(&mut self.layout, Some(landed)).unwrap_or(Pos::new(0, 0));
        self.edited();
        // What is still on the pages stays picked (not what went into a folder).
        self.org.picks.prune(&self.layout);
        self.arr.drop_at = Some((key.clone(), at));
        let i = self.flat(landed);
        self.sel = Some(i);
        // The cells bound again in place: the rest glide home.
        self.refresh_pages(landed.page, true);
        self.arr.drop_at = None;
        // Where it landed as itself (not into a folder), it comes home.
        let cell = self.pages.get(&landed.page).filter(|v| v.bound.get(landed.slot).and_then(|k| k.as_deref()) == Some(key.as_str())).and_then(|v| v.cells.get(landed.slot)).map(|c| c.0.clone());
        let home = self.land(cell, self.icon_rect(&self.grid, landed.slot), true);
        self.drag_gone(home);
        if new_folder {
            // A new folder opens, its name ready to change.
            self.open_folder(i);
            self.start_rename(false);
        } else if let Some(t_) = self.item_at(i).and_then(|it| self.item_tile(it)) {
            self.announce(tf("launchpad.edit.dropped", &[("name", &t_.name)]));
        }
    }

    /// A dropped icon comes home (6.2): its cell's icon `cell` (at `r`: in
    /// the grid's own coordinates, `in_grid`, or on screen) glides from the
    /// size it was dragged at back to its own, as bind has it glide from
    /// where it was let go. Where the dragged visual's shadows go with it
    /// (drag_gone); None: nothing glides.
    fn land(&mut self, cell: Option<SpriteVisual>, r: Rect, in_grid: bool) -> Option<(Vector3, f32)> {
        let cell = cell.filter(|_| self.calm_motion())?;
        let _ = glide(&self.gfx.compositor, &cell, "Scale", Some(v3(edit::LIFT_SCALE)), v3(1.0));
        let (k, (cx, cy)) = if in_grid { (self.shown_scale(), self.from_grid(r.cx(), r.cy())) } else { (1.0, (r.cx(), r.cy())) };
        let e = self.arr.drag_e;
        Some((Vector3 { X: cx - e / 2.0, Y: cy - e / 2.0, Z: 0.0 }, k))
    }

    /// The pages changed: bound again in place (`in_place`; views past the
    /// last page go), else all made again; page `page` shown.
    fn refresh_pages(&mut self, page: usize, in_place: bool) {
        let n = self.pages_n();
        if in_place {
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
        } else {
            // (The Library's visuals go too: made again where it is now.)
            self.drop_cells();
        }
        self.go_page(page.min(n - 1), true);
        self.update_sel();
    }

    /// A drag ends without a drop (Esc, the capture lost, closing): the
    /// pages as they were.
    pub(super) fn arr_cancel_drag(&mut self) {
        let mut back = None;
        if let Some(d) = self.arr.drag.take() {
            back = Some(d.before);
        }
        if let Some(fd) = self.arr.folder.as_mut().and_then(|f| f.drag.take()) {
            back = Some(fd.before);
        }
        // (A press on Done ends with the capture too.)
        self.done_to(State::Rest);
        let Some(before) = back else { return };
        self.layout = before;
        self.clear_feedback();
        self.drag_gone(None);
        unsafe {
            let _ = ReleaseCapture();
        }
        if self.arr.folder.is_some() {
            self.bind_folder();
        }
        let p = self.page.min(self.layout.len().saturating_sub(1));
        self.refresh_pages(p, false);
    }

    // ---- the keyboard ------------------------------------------------------------

    /// Keys in edit mode and on the open folder. True: ours.
    pub(super) fn arr_key(&mut self, vk: u16) -> bool {
        // While a press or a drag is under way the page holds still: what it
        // holds (or will lift) is on this page.
        let held = self.press.is_some() || self.arr.drag.is_some() || self.arr.folder.as_ref().is_some_and(|f| f.drag.is_some());
        let moves = [VK_PRIOR.0, VK_NEXT.0, VK_HOME.0, VK_END.0, VK_LEFT.0, VK_RIGHT.0, VK_UP.0, VK_DOWN.0];
        if held && self.arr.rename.is_none() && moves.contains(&vk) {
            return true;
        }
        if self.arr.rename.is_some() {
            self.rename_key(vk);
            return true;
        }
        if self.arr.folder.is_some() {
            return self.folder_key(vk);
        }
        if !self.arr.edit {
            return false;
        }
        match vk {
            v if v == VK_ESCAPE.0 => {
                if self.arr.drag.is_some() {
                    self.arr_cancel_drag();
                } else if self.arr.lifted.take().is_some() {
                    self.say_item(self.sel, "launchpad.edit.dropped");
                } else if !self.org.picks.is_empty() {
                    // Picked icons let go first; Esc again ends edit mode.
                    self.clear_picks();
                    self.announce(t("launchpad.edit.picks_cleared").to_string());
                } else {
                    self.end_edit();
                }
                true
            }
            // Ctrl+Space: the focused icon picked or not (organize.rs).
            v if v == VK_SPACE.0 && key_down(VK_CONTROL) && self.focus == Focus::Grid && !self.searching => {
                self.eat_space = true;
                if self.arr.lifted.is_none() {
                    if let Some(i) = self.sel.filter(|&i| self.valid_sel(i)) {
                        self.toggle_pick_at(i);
                    }
                }
                true
            }
            v if v == VK_SPACE.0 && self.focus == Focus::Grid && !self.searching => {
                self.eat_space = true;
                if self.arr.lifted.take().is_some() {
                    self.say_item(self.sel, "launchpad.edit.dropped");
                } else if let Some(i) = self.sel.filter(|&i| self.valid_sel(i)) {
                    self.arr.lifted = Some(self.pos_of(i));
                    // A picked icon takes the others along (as a drag
                    // does); another one goes alone and nothing stays
                    // picked (organize.rs).
                    let n = self.org.picks.len();
                    if self.is_picked(i) && n >= 2 {
                        self.announce(tf("launchpad.edit.lifted_n", &[("count", &n.to_string())]));
                    } else {
                        self.clear_picks();
                        self.say_item(Some(i), "launchpad.edit.lifted");
                    }
                }
                true
            }
            v if v == VK_RETURN.0 && self.focus == Focus::Grid => {
                if self.arr.lifted.take().is_some() {
                    self.say_item(self.sel, "launchpad.edit.dropped");
                } else if let Some(i) = self.sel.filter(|&i| matches!(self.item_at(i), Some(LpItem::Folder { .. }))) {
                    self.open_folder(i);
                }
                true
            }
            v if self.arr.lifted.is_some() && [VK_LEFT.0, VK_RIGHT.0, VK_UP.0, VK_DOWN.0].contains(&v) => {
                let a = match v {
                    x if x == VK_LEFT.0 => Arrow::Left,
                    x if x == VK_RIGHT.0 => Arrow::Right,
                    x if x == VK_UP.0 => Arrow::Up,
                    _ => Arrow::Down,
                };
                self.shift_lifted(a);
                true
            }
            _ => false,
        }
    }

    /// The keyboard moves the lifted icon one place.
    fn shift_lifted(&mut self, a: Arrow) {
        if self.searching {
            self.arr.lifted = None;
            return;
        }
        let Some(at) = self.arr.lifted else { return };
        let (g, rtl, pp) = (self.grid, self.rtl, self.per_page());
        // The picked ones together (organize.rs), as a drag moves them;
        // an icon not picked alone, and nothing stays picked.
        let key = self.layout.get(at.page).and_then(|p| p.get(at.slot)).map(super::super::picks::key_of);
        let many = self.org.picks.len() >= 2 && key.as_ref().is_some_and(|k| self.org.picks.contains(k));
        if !many {
            self.clear_picks();
        }
        let moved = if many {
            let ps = self.org.picks.positions(&self.layout);
            super::super::picks::move_many(&mut self.layout, &ps, at, &g, a, rtl, pp)
        } else {
            edit::shift(&mut self.layout, at, &g, a, rtl, pp)
        };
        let Some(to) = moved else { return };
        self.edited();
        self.arr.lifted = Some(to);
        let i = self.flat(to);
        self.sel = Some(i);
        self.refresh_pages(to.page, true);
        if let Some(t_) = self.item_at(i).and_then(|it| self.item_tile(it)) {
            let n = (to.slot + 1).to_string();
            let page = (to.page + 1).to_string();
            self.announce(tf("launchpad.edit.moved", &[("name", &t_.name), ("page", &page), ("n", &n)]));
        }
    }

    /// A screen reader's Move earlier / Move later (the selected icon).
    pub(super) fn arr_move_sel(&mut self, later: bool) {
        let Some(i) = self.sel.filter(|&i| self.arr.edit && !self.searching && self.valid_sel(i)) else { return };
        let keep = self.arr.lifted;
        self.arr.lifted = Some(self.pos_of(i));
        self.shift_lifted(if later != self.rtl { Arrow::Right } else { Arrow::Left });
        if keep.is_none() {
            self.arr.lifted = None;
        }
    }

    fn say_item(&mut self, i: Option<usize>, key: &str) {
        if let Some(t_) = i.and_then(|i| self.item_at(i)).and_then(|it| self.item_tile(it)) {
            self.announce(tf(key, &[("name", &t_.name)]));
        }
    }

    /// A typed character: the folder's new name; else edit mode (and the
    /// folder) end and it goes to the search field. True: ours.
    pub(super) fn arr_char(&mut self, c: char) -> bool {
        if let Some(f) = self.arr.rename.as_mut() {
            f.insert(&c.to_string());
            self.draw_panel();
            return true;
        }
        if c == ' ' && (self.arr.edit || self.arr.folder.is_some()) {
            return true;
        }
        if self.arr.edit || self.arr.folder.is_some() {
            self.arr_leave_for_typing();
        }
        false
    }

    // ---- the open folder ---------------------------------------------------------

    /// The open folder's app ids (as on the pages; with a drag, the others).
    pub(super) fn folder_apps(&self) -> Vec<String> {
        let Some(f) = &self.arr.folder else { return Vec::new() };
        if let Some((c, _)) = f.cat {
            return self.lib.groups.iter().find(|g| g.cat == c).map(|g| g.apps.clone()).unwrap_or_default();
        }
        if let Some(d) = &f.drag {
            return d.rest.clone();
        }
        match self.layout.get(f.at.page).and_then(|p| p.get(f.at.slot)) {
            Some(LpItem::Folder { apps, .. }) => apps.iter().filter(|a| self.by_id.contains_key(*a)).cloned().collect(),
            _ => Vec::new(),
        }
    }

    pub(super) fn folder_name(&self) -> String {
        let Some(f) = &self.arr.folder else { return String::new() };
        if let Some((c, _)) = f.cat {
            return t(c.label_key()).to_string();
        }
        match self.layout.get(f.at.page).and_then(|p| p.get(f.at.slot)) {
            Some(LpItem::Folder { name, .. }) => folders::shown_name(name, t("launchpad.folder.default")),
            _ => String::new(),
        }
    }

    /// Opens the folder at listing index `i` in a panel over the grid.
    pub(super) fn open_folder(&mut self, i: usize) {
        if self.arr.folder.is_some() {
            self.close_folder();
        }
        let at = self.pos_of(i);
        let Some(LpItem::Folder { apps, .. }) = self.layout.get(at.page).and_then(|p| p.get(at.slot)) else { return };
        let n = apps.len();
        self.sel = Some(i);
        self.open_panel(at, None, n);
    }

    /// Opens Library tile `k`'s category in the folder's panel (the same
    /// look and motion), read only.
    pub(super) fn open_category(&mut self, k: usize) {
        if self.arr.edit {
            self.end_edit();
        }
        if self.arr.folder.is_some() {
            self.close_folder();
        }
        let Some(g) = self.lib.groups.get(k) else { return };
        let (cat, n) = (g.cat, g.apps.len());
        self.lib.sel = Some(k);
        self.open_panel(Pos::new(usize::MAX, 0), Some((cat, k)), n);
    }

    /// The panel for `n` apps over the grid (a folder at `at`, or a category).
    fn open_panel(&mut self, at: Pos, cat: Option<(super::super::library::Cat, usize)>, n: usize) {
        let pg = folders::panel_grid(n, &self.grid, self.work.w(), self.work.h());
        // Opened from the keyboard, its first app is the keyboard's place;
        // opened by the pointer, nothing is lit until the pointer is on one.
        let keyboard = self.focus == Focus::Grid && self.press.is_none() && self.by_key;
        self.arr.folder = Some(OpenFolder { at, cat, pg, scroll: 0, sel: keyboard.then_some(0), drag: None });
        let Some(view) = self.make_cells(&pg, pg.cols * pg.rows, 0.0) else {
            self.arr.folder = None;
            return;
        };
        if self.arr.folder_box.Children().and_then(|ch| ch.InsertAtTop(&view.container)).is_err() {
            self.arr.folder = None;
            return;
        }
        self.arr.folder_view = Some(view);
        self.draw_panel();
        self.bind_folder();
        self.drop_leaving();
        let _ = self.arr.folder_box.SetIsVisible(true);
        let c = self.gfx.compositor.clone();
        if self.animate() {
            // It grows from its folder's tile (a category's panel from its
            // own middle) and fades in: the panels' motion (crate::motion).
            let r = folders::panel_rect(&pg).offset(self.work.l, self.work.t);
            let from = match cat {
                None => {
                    let t = self.icon_rect(&self.grid, at.slot);
                    (t.cx(), t.cy())
                }
                Some(_) => (r.cx(), r.cy()),
            };
            let _ = crate::motion::open(&c, &self.arr.folder_box, (self.w, self.h), from, crate::motion::Kind::Panel, 1.0);
        } else {
            // Not animating: nothing grows, it fades in (12.2).
            set_scale(&self.arr.folder_box, 1.0);
            fade_v(&c, &self.arr.folder_box, Some(0.0), 1.0, self.panel_fade(true));
        }
        self.dim_behind(true);
        self.update_sel();
        self.draw_folder_sel(true);
        let name = self.folder_name();
        let key = if cat.is_some() { "launchpad.library.a11y" } else { "launchpad.folder.a11y" };
        self.announce(tf(key, &[("name", &name), ("count", &n.to_string())]));
    }

    pub(super) fn close_folder(&mut self) {
        if self.arr.rename.is_some() {
            self.commit_rename();
        }
        if self.arr.folder.as_ref().is_some_and(|f| f.drag.is_some()) {
            // Its drag ends here: feedback and timers first.
            self.clear_feedback();
            unsafe {
                let _ = ReleaseCapture();
            }
        }
        if let Some(fd) = self.arr.folder.as_mut().and_then(|f| f.drag.take()) {
            self.layout = fd.before;
            self.drag_gone(None);
        }
        self.arr.border_lit = false;
        // (Its cells back in the box: they fade out from there.)
        self.unedge_folder();
        let Some(f) = self.arr.folder.take() else { return };
        self.drop_leaving();
        if let Some(v) = self.arr.folder_view.take() {
            let _ = self.arr.folder_box.Children().and_then(|ch| ch.Remove(&v.container));
            let _ = self.fade_leaving(&v.container);
        }
        let _ = self.arr.folder_box.SetIsVisible(false);
        self.sel_off(true);
        self.dim_behind(false);
        if let Some((_, k)) = f.cat {
            // Back on its tile.
            self.lib.sel = Some(k);
            self.update_sel();
            self.publish();
            return;
        }
        // Its tile shows a new name or other apps now.
        if self.pages.contains_key(&f.at.page) {
            self.bind_page(f.at.page);
        }
        self.sel = Some(self.flat(f.at)).filter(|&i| self.valid_sel(i));
        self.update_sel();
        self.publish();
    }

    /// A closing panel's fade (120, as a panel closes; not animating 60 ms
    /// straight): its plate (as drawn) and its icons move to a copy over
    /// the box, which fades out (the box itself is free for the next
    /// folder at once).
    fn fade_leaving(&mut self, icons: &ContainerVisual) -> Result<()> {
        let c = &self.gfx.compositor;
        let ghost = c.CreateContainerVisual()?;
        let card = c.CreateSpriteVisual()?;
        card.SetBrush(&self.arr.panel_v.Brush()?)?;
        card.SetOffset(self.arr.panel_v.Offset()?)?;
        card.SetSize(self.arr.panel_v.Size()?)?;
        ghost.Children()?.InsertAtTop(&card)?;
        ghost.Children()?.InsertAtTop(icons)?;
        let parent = self.arr.folder_box.Parent()?;
        parent.Children()?.InsertAbove(&ghost, &self.arr.folder_box)?;
        ghost.StartAnimation(&HSTRING::from("Opacity"), &fade(c, None, 0.0, self.panel_fade(false))?)?;
        self.arr.leaving = Some(ghost);
        Ok(())
    }

    /// The last closed panel's copy goes (faded out by now, or cut short).
    fn drop_leaving(&mut self) {
        if let Some(g) = self.arr.leaving.take() {
            if let Ok(p) = g.Parent() {
                let _ = p.Children().and_then(|ch| ch.Remove(&g));
            }
        }
    }

    /// The grid behind an open folder fades (and comes back) as the plate
    /// does: SCRIM_MS in, SCRIM_OUT_MS out, straight (7.1).
    fn dim_behind(&self, dim: bool) {
        let to = if dim { edit::BEHIND_FOLDER } else { 1.0 };
        let plan = super::motion::scrim(self.animate(), dim, crate::effects::current().speed);
        fade_v(&self.gfx.compositor, &self.grid_box, None, to, plan);
    }

    /// The panel (paint_panel): the big plate with the folder's name (a
    /// field with its caret while it is being renamed).
    pub(super) fn draw_panel(&mut self) {
        let Some(f) = &self.arr.folder else { return };
        let r = folders::panel_rect(&f.pg).offset(self.work.l, self.work.t);
        let empty = f.drag.is_none() && self.folder_apps().is_empty();
        let renaming = self.arr.rename.as_ref().map(|fl| fl.with_composition(&self.comp));
        let caret = self.arr.rename.as_ref().map(|fl| if self.comp.is_empty() { fl.caret() } else { fl.caret() + self.comp.len() });
        let text: String = match &renaming {
            Some((chars, _)) => chars.iter().collect(),
            None => self.folder_name(),
        };
        let name = name_layout(&self.gfx, &self.dw, &self.theme, &text, r.w() - 2.0 * folders::PANEL_INK, self.rtl);
        let row = match (&renaming, caret) {
            (Some((chars, _)), Some(c)) => {
                let x = name.as_ref().map(|(l, _)| Self::x_at(l, crate::search::utf16_len(&chars[..c.min(chars.len())])));
                NameRow::Typing { caret: x, ring: self.arr.rename_ring }
            }
            _ => NameRow::Plain,
        };
        let shadows = crate::effects::items().shadows && self.hc().is_none();
        let look = PanelLook { name: name.as_ref().map(|(l, h)| (l, *h)), row, lit: self.arr.border_lit, empty: empty.then(|| t("dock.stack.empty")), shadows, coat: panel_coat(&self.theme, self.s) };
        // (Room for what the plate casts: E3 reaches SHADOW_M.)
        let m = if shadows { crate::theme::metrics::SHADOW_M } else { space::S1 };
        let v = self.arr.panel_v.clone();
        let (gfx, theme) = (&self.gfx, &self.theme);
        self.paint_with_room(&v, r, m, |dc| unsafe { paint_panel(gfx, dc, r.w(), r.h(), theme, &look) });
    }

    /// The open folder's cells show its apps (from the scrolled row, with
    /// the gap a dragged app makes).
    pub(super) fn bind_folder(&mut self) {
        let Some(f) = &self.arr.folder else { return };
        let apps = self.folder_apps();
        let gap = f.drag.as_ref().map(|d| d.gap);
        let first = f.scroll * f.pg.cols;
        let n = f.pg.cols * f.pg.rows;
        let bands = folder_bands(apps.len(), &f.pg, f.scroll);
        let entries: Vec<Option<Tile>> = (0..n).map(|s| edit::item_in_slot(first + s, gap).and_then(|k| apps.get(k)).and_then(|id| self.app_by_id(id)).map(|a| self.app_tile(a))).collect();
        self.bind(Cells::Folder, entries, true);
        self.edge_folder(bands);
    }

    /// The open folder's scroll edge as `bands` says (None: none; 2.5).
    /// Made the first time it is needed, kept while it scrolls; its bands
    /// come and go as Edges::set fades them (90 / 120: not motion, so
    /// also when not animating).
    fn edge_folder(&mut self, bands: Option<(bool, bool)>) {
        let hc = self.hc().is_some();
        match (bands, self.arr.folder_edges.as_mut()) {
            (Some(b), Some(e)) => e.edges.set(&self.gfx.compositor, b, hc),
            (None, Some(_)) => self.unedge_folder(),
            (None, None) => {}
            (Some(b), None) => {
                let (Some(f), Some(cells)) = (&self.arr.folder, self.arr.folder_view.as_ref().map(|v| v.container.clone())) else { return };
                let area = on_px(folder_scroll_area(&f.pg).offset(self.work.l, self.work.t), self.s);
                let _ = self.arr.folder_box.Children().and_then(|ch| ch.Remove(&cells));
                let made = folder_edges(&self.gfx, &self.theme, &cells, area, self.s, b).and_then(|e| {
                    self.arr.folder_box.Children()?.InsertAtTop(&e.edges.stage)?;
                    Ok(e)
                });
                match made {
                    Ok(e) => self.arr.folder_edges = Some(e),
                    Err(e) => {
                        // (No edge: the cells as they were.)
                        crate::log!("launchpad: the folder's scroll edge failed: {e:?}");
                        if let Ok(p) = cells.Parent() {
                            let _ = p.Children().and_then(|ch| ch.Remove(&cells));
                        }
                        let _ = cells.SetOffset(Vector3 { X: 0.0, Y: 0.0, Z: 0.0 });
                        let _ = self.arr.folder_box.Children().and_then(|ch| ch.InsertAtTop(&cells));
                    }
                }
            }
        }
    }

    /// The open folder's cells back in its box as they were (no edge).
    pub(super) fn unedge_folder(&mut self) {
        let Some(e) = self.arr.folder_edges.take() else { return };
        let _ = self.arr.folder_box.Children().and_then(|ch| ch.Remove(&e.edges.stage));
        if let Some(cells) = self.arr.folder_view.as_ref().map(|v| v.container.clone()) {
            let _ = e.viewport.Children().and_then(|ch| ch.Remove(&cells));
            let _ = cells.SetOffset(Vector3 { X: 0.0, Y: 0.0, Z: 0.0 });
            let _ = self.arr.folder_box.Children().and_then(|ch| ch.InsertAtTop(&cells));
        }
    }

    /// What is lit in the open folder: its app with the keyboard's place or
    /// under the pointer, whichever moved last (select.rs). `say`: its name
    /// is said (the keyboard came to it).
    pub(super) fn draw_folder_sel(&mut self, say: bool) {
        let Some(f) = &self.arr.folder else { return };
        let Some(i) = f.sel.filter(|_| super::select::pointer_shows(self.by_key, self.pointer_out)) else {
            self.sel_off(self.by_key);
            return;
        };
        let (pg, scroll) = (f.pg, f.scroll);
        let slot = i.checked_sub(scroll * pg.cols).filter(|&s| s < pg.cols * pg.rows);
        let container = self.arr.folder_view.as_ref().map(|v| v.container.clone());
        let (Some(slot), Some(container)) = (slot, container) else {
            // (Scrolled out of view: nothing is lit.)
            self.sel_off(true);
            return;
        };
        let cell = pg.item(slot, self.rtl).offset(self.work.l, self.work.t);
        let icon = self.icon_rect(&pg, slot);
        self.sel_on(Some(usize::MAX), &container, cell, icon, icon.w() * crate::theme::SQUIRCLE_ARC);
        if !say {
            return;
        }
        if let Some(name) = self.folder_apps().get(i).and_then(|id| self.app_by_id(id)).map(|a| a.name.clone()) {
            self.announce(name);
        }
    }

    /// The pointer over app `i` of the open folder: it is lit there, as on
    /// Home (6.2: one thing at a time; the keyboard's ring goes).
    pub(super) fn folder_hover(&mut self, i: usize, again: bool) {
        let by_key = self.by_key;
        let Some(f) = self.arr.folder.as_mut() else { return };
        if f.drag.is_some() || (f.sel == Some(i) && !by_key && !again) {
            return;
        }
        f.sel = Some(i);
        self.by_key = false;
        self.draw_folder_sel(false);
    }

    /// App `i` of the open folder takes the keyboard's place (scrolled into
    /// view, said aloud): arrow keys, a screen reader's SetFocus.
    pub(super) fn folder_select(&mut self, i: usize) {
        let Some(f) = self.arr.folder.as_mut() else { return };
        let row = i / f.pg.cols.max(1);
        f.sel = Some(i);
        if row < f.scroll {
            f.scroll = row;
        } else if row >= f.scroll + f.pg.rows {
            f.scroll = row + 1 - f.pg.rows;
        }
        self.bind_folder();
        self.draw_folder_sel(true);
        self.publish();
    }

    /// The wheel over an open folder scrolls its rows.
    pub(super) fn folder_wheel(&mut self, delta: i32) {
        let n = self.folder_apps().len();
        let Some(f) = self.arr.folder.as_mut() else { return };
        let max = folders::max_scroll(n, &f.pg);
        let to = if delta < 0 { (f.scroll + 1).min(max) } else { f.scroll.saturating_sub(1) };
        if to != f.scroll {
            f.scroll = to;
            let last = f.drag.as_ref().map(|d| d.last);
            // A drag: what the pointer is over changed; aimed again from
            // where it is (the copy moves, or hides out of view).
            if last.is_some() {
                self.stop_settle();
            }
            self.bind_folder();
            // (What is lit goes with its app, or out of view with it.)
            self.draw_folder_sel(false);
            if let Some((x, y)) = last {
                self.folder_drag_move(x, y);
                self.place_ghost();
            }
        }
    }

    /// An app in the open folder was clicked (not in edit mode): it opens.
    pub(super) fn launch_from_folder(&mut self, i: usize) {
        let Some(app) = self.folder_apps().get(i).and_then(|id| self.app_by_id(id)).cloned() else { return };
        self.lib_seen(&app.id);
        self.finish(Some(After::Open(app.target())));
    }

    fn folder_key(&mut self, vk: u16) -> bool {
        let n = self.folder_apps().len();
        let Some(f) = self.arr.folder.as_ref() else { return false };
        let (pg, sel) = (f.pg, f.sel);
        match vk {
            v if v == VK_ESCAPE.0 => {
                if f.drag.is_some() {
                    self.arr_cancel_drag();
                } else {
                    self.close_folder();
                }
                true
            }
            v if v == VK_F2.0 && self.arr.edit => {
                self.start_rename(true);
                true
            }
            v if v == VK_RETURN.0 || v == VK_SPACE.0 => {
                self.eat_space = v == VK_SPACE.0;
                if let Some(i) = sel.filter(|&i| i < n) {
                    if !self.arr.edit {
                        self.launch_from_folder(i);
                    }
                }
                true
            }
            v if [VK_LEFT.0, VK_RIGHT.0, VK_UP.0, VK_DOWN.0].contains(&v) => {
                let a = match v {
                    x if x == VK_LEFT.0 => Arrow::Left,
                    x if x == VK_RIGHT.0 => Arrow::Right,
                    x if x == VK_UP.0 => Arrow::Up,
                    _ => Arrow::Down,
                };
                // The folder's rows as one long page.
                let tall = Grid { rows: n.div_ceil(pg.cols.max(1)).max(1), ..pg };
                let next = match sel {
                    None => (n > 0).then_some(0),
                    Some(i) => grid::step(i, n, &tall, a, self.rtl),
                };
                if let Some(i) = next {
                    self.folder_select(i);
                }
                true
            }
            // The selected app's menu (Home, the Library, the Dock).
            v if v == VK_APPS.0 || (v == VK_F10.0 && key_down(VK_SHIFT)) => {
                if let Some(i) = sel.filter(|&i| i < n) {
                    let slot = i.saturating_sub(f.scroll * pg.cols);
                    let r = pg.icon(slot, self.rtl).offset(self.work.l, self.work.t);
                    self.panel_menu(i, (r.cx(), r.cy()), true);
                }
                true
            }
            v if v == VK_TAB.0 => true,
            _ => false,
        }
    }

    /// App `i` of the open folder or category: its menu (not in edit mode).
    pub(super) fn panel_menu(&mut self, i: usize, at: (f32, f32), keyboard: bool) {
        if self.arr.edit || self.arr.rename.is_some() {
            return;
        }
        let Some(app) = self.folder_apps().get(i).and_then(|id| self.app_by_id(id)).cloned() else { return };
        if let Some(f) = self.arr.folder.as_mut() {
            f.sel = Some(i);
        }
        self.open_app_menu(app, at, keyboard, false);
    }

    // ---- renaming --------------------------------------------------------------

    /// The open folder's name becomes a field (`by_key`: the keyboard
    /// asked, so the field carries its ring).
    pub(super) fn start_rename(&mut self, by_key: bool) {
        if self.arr.folder.is_none() || !self.arr.edit {
            return;
        }
        self.arr.rename_ring = by_key;
        // A composition of the search field's never goes into the name.
        if !self.comp.is_empty() {
            self.comp.clear();
            self.drop_ime();
        }
        let mut f = Field::default();
        f.insert(&self.folder_name());
        f.select_all();
        self.arr.rename = Some(f);
        self.draw_panel();
        self.announce(tf("launchpad.folder.rename", &[("name", &self.folder_name())]));
    }

    /// The IME's open composition is cancelled; what it still sends for it
    /// is ignored (view.rs on_composition).
    pub(super) fn drop_ime(&mut self) {
        self.arr.ime_dropped = true;
        unsafe {
            let himc = ImmGetContext(self.hwnd);
            if !himc.is_invalid() {
                let _ = ImmNotifyIME(himc, NI_COMPOSITIONSTR, CPS_CANCEL, 0);
                let _ = ImmReleaseContext(self.hwnd, himc);
            }
        }
    }

    /// The name typed is kept (empty: the default name shows), with what
    /// was still being composed in it (as shown): the composition is taken
    /// into this rename, then the IME's is cancelled, so nothing of it
    /// arrives later anywhere else.
    pub(super) fn commit_rename(&mut self) {
        let Some(mut f) = self.arr.rename.take() else { return };
        if !self.comp.is_empty() {
            let c: String = self.comp.iter().collect();
            f.insert(&c);
            self.comp.clear();
            self.drop_ime();
        }
        let name = folders::clean_name(&f.text());
        if let Some(at) = self.arr.folder.as_ref().map(|f| f.at) {
            if let Some(LpItem::Folder { name: n, auto, .. }) = self.layout.get_mut(at.page).and_then(|p| p.get_mut(at.slot)) {
                if *n != name {
                    *n = name;
                    // (Renamed: the user's folder now, not Tidy Home's.)
                    *auto = false;
                    self.edited();
                }
            }
        }
        self.draw_panel();
    }

    fn rename_key(&mut self, vk: u16) {
        let (ctrl, shift) = (key_down(VK_CONTROL), key_down(VK_SHIFT));
        let rtl = self.rtl;
        let Some(f) = self.arr.rename.as_mut() else { return };
        match vk {
            v if v == VK_RETURN.0 => {
                self.commit_rename();
                return;
            }
            v if v == VK_ESCAPE.0 => {
                self.arr.rename = None;
                if !self.comp.is_empty() {
                    self.comp.clear();
                    self.drop_ime();
                }
            }
            v if v == VK_BACK.0 => f.backspace(ctrl),
            v if v == VK_DELETE.0 => f.delete(ctrl),
            v if v == VK_LEFT.0 || v == VK_RIGHT.0 => {
                if (v == VK_LEFT.0) != rtl { f.left(ctrl, shift) } else { f.right(ctrl, shift) }
            }
            v if v == VK_HOME.0 => f.home(shift),
            v if v == VK_END.0 => f.end(shift),
            v if ctrl && v == b'A' as u16 => f.select_all(),
            v if v == VK_SPACE.0 => {}
            _ => return,
        }
        self.draw_panel();
    }

    // ---- dragging inside the open folder --------------------------------------------

    /// Picks up app `i` of the open folder (edit mode).
    fn folder_lift(&mut self, i: usize, at: (f32, f32)) {
        let apps = self.folder_apps();
        let Some(id) = apps.get(i).cloned() else { return };
        let Some(app) = self.app_by_id(&id).cloned() else { return };
        let Some(f) = &self.arr.folder else { return };
        let (pg, scroll) = (f.pg, f.scroll);
        let Some(slot) = i.checked_sub(scroll * pg.cols).filter(|&s| s < pg.cols * pg.rows) else { return };
        let ir = self.icon_rect(&pg, slot);
        let key = app.target();
        let brush = self.icon_brush(&key).or_else(|| self.placeholder());
        // (The panel is not scaled.)
        let tl = self.show_dragged(brush, (ir.cx(), ir.cy()), ir.w(), 1.0);
        let mut rest = apps;
        rest.remove(i);
        let before = self.layout.clone();
        if let Some(f) = self.arr.folder.as_mut() {
            f.drag = Some(FolderDrag { id, key, rest, gap: i, grab: (at.0 - tl.0, at.1 - tl.1), start: at, moved: false, before, last: at });
            f.sel = None;
        }
        self.arr.drag_gen += 1;
        self.press = None;
        self.sel_off(true);
        unsafe {
            SetCapture(self.hwnd);
        }
        self.bind_folder();
        self.place_ghost();
        self.announce(tf("launchpad.edit.lifted", &[("name", &app.name)]));
    }

    fn folder_drag_move(&mut self, x: f32, y: f32) {
        let Some(f) = self.arr.folder.as_ref() else { return };
        let Some(d) = f.drag.as_ref() else { return };
        let (pg, scroll, gap, n) = (f.pg, f.scroll, d.gap, d.rest.len());
        let at = Vector3 { X: x - d.grab.0, Y: y - d.grab.1, Z: 0.0 };
        let moved = d.moved || dist((x, y), d.start) > 2.0;
        let _ = self.arr.drag_v.SetOffset(at);
        if let Some(d) = self.arr.folder.as_mut().and_then(|f| f.drag.as_mut()) {
            d.moved = moved;
            d.last = (x, y);
        }
        let panel = folders::panel_rect(&pg).offset(self.work.l, self.work.t);
        // Near the border it lights; past it the app leaves the folder.
        let lit = match edit::border((panel.l, panel.t, panel.r, panel.b), x, y) {
            edit::Border::Out => {
                self.drag_out_of_folder(x, y);
                return;
            }
            edit::Border::Near => true,
            edit::Border::Inside => false,
        };
        if lit != self.arr.border_lit {
            self.arr.border_lit = lit;
            self.draw_panel();
        }
        let shown = self.cell_under(&pg, pg.cols * pg.rows, x, y).map(|(s, z)| (s + scroll * pg.cols, z));
        // The same reorder intent as on the pages (no folders in folders:
        // the middle of an app keeps the gap).
        match edit::aim(shown, gap, n) {
            Aim::Gap(g) => self.aim_gap(gap, g),
            Aim::Onto(_) => self.stop_settle(),
        }
    }

    /// Let go inside the panel: the app goes in at the gap.
    fn folder_drop(&mut self) {
        let pending = self.arr.pending;
        let dropped = self.dropped_at(false);
        self.clear_feedback();
        self.draw_panel();
        let Some(f) = self.arr.folder.as_mut() else { return };
        let Some(d) = f.drag.take() else { return };
        let (at, pg, scroll) = (f.at, f.pg, f.scroll);
        if !d.moved {
            self.drag_gone(None);
            self.layout = d.before;
            self.bind_folder();
            return;
        }
        let mut apps = d.rest;
        apps.push(d.id);
        let last = apps.len() - 1;
        let to = pending.unwrap_or(d.gap).min(last);
        folders::move_in(&mut apps, last, to);
        if let Some(LpItem::Folder { apps: a, .. }) = self.layout.get_mut(at.page).and_then(|p| p.get_mut(at.slot)) {
            if *a != apps {
                *a = apps;
                self.edited();
            }
        }
        self.arr.drop_at = Some((d.key, dropped));
        self.bind_folder();
        self.arr.drop_at = None;
        // It comes home to its cell (if that is in view).
        let slot = to.checked_sub(scroll * pg.cols).filter(|&s| s < pg.cols * pg.rows);
        let cell = slot.and_then(|s| self.arr.folder_view.as_ref().and_then(|v| v.cells.get(s)).map(|c| c.0.clone()));
        let home = slot.and_then(|s| self.land(cell, self.icon_rect(&pg, s), false));
        self.drag_gone(home);
    }

    /// Dragged out of the panel: out of the folder (one app left: the
    /// folder is that app again), the panel closes, and the drag goes on
    /// over the pages.
    fn drag_out_of_folder(&mut self, x: f32, y: f32) {
        let Some(f) = self.arr.folder.as_mut() else { return };
        let Some(d) = f.drag.take() else { return };
        let at = f.at;
        // The folder without the app (its other apps in their new order).
        if let Some(LpItem::Folder { apps, .. }) = self.layout.get_mut(at.page).and_then(|p| p.get_mut(at.slot)) {
            *apps = d.rest.clone();
            apps.push(d.id.clone());
        }
        let _ = folders::take_out(&mut self.layout, at, d.rest.len());
        self.edited();
        self.clear_feedback();
        self.close_folder();
        let page = at.page.min(self.layout.len().saturating_sub(1));
        let gap = self.page_len(page);
        self.arr.drag = Some(IconDrag { item: LpItem::App(d.id), others: Vec::new(), lead_at: 0, key: d.key, page, gap, grab: d.grab, start: d.start, moved: true, before: d.before });
        self.arr.drag_gen += 1;
        let _ = self.arr.drag_v.SetIsVisible(true);
        self.bind_page(page);
        self.ensure_neighbours();
        self.place_ghost();
        self.drag_move(x, y);
    }
}

/// A folder's tile (paint_folder_tile), drawn at `px` device pixels (its
/// icons with D2D's cubic filter).
impl Pad {
    pub(super) fn folder_brush(&mut self, key: &str, apps: &[String]) -> Option<CompositionSurfaceBrush> {
        if let Some(b) = self.arr.folder_brushes.get(key) {
            return Some(b.clone());
        }
        let src = self.src_px();
        let icons: Vec<Option<std::sync::Arc<crate::icons::Icon>>> = folders::mini(apps).iter().map(|k| loader::cached(k, src).flatten()).collect();
        let complete = folders::mini(apps).iter().all(|k| loader::cached(k, src).is_some());
        let (s, px, e, rtl) = (self.s, self.px, self.icon_dip(), self.rtl);
        let theme = &self.theme;
        let surface = self
            .gfx
            .draw(px, px, |dc, ox, oy| unsafe {
                dc.SetTransform(&Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: ox, M32: oy });
                let res = paint_folder_tile(dc, e, &icons, theme, rtl);
                dc.SetTransform(&Matrix3x2::identity());
                res
            })
            .ok()?;
        let b = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface).ok()?;
        let _ = b.SetStretch(CompositionStretch::Fill);
        if complete {
            self.arr.folder_brushes.insert(key.to_string(), b.clone());
        }
        Some(b)
    }
}

/// Pictures for the design audit (tests only).
#[cfg(test)]
#[path = "arrange_harmony.rs"]
mod harmony;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_folder_panel_wears_a_panels_plate_grain_and_all() {
        use crate::syslook::SysLook;
        use crate::theme::{Finish, Material};
        for light in [false, true] {
            let sys = SysLook { light, ..Default::default() };
            // The Flat one chose: the panels' fall, their grain and the
            // 1 DIP rim, in the one picture.
            let t = Theme::graphite().resolve_fx(&sys, true);
            for s in [1.0, 1.5, 2.0] {
                let c = panel_coat(&t, s);
                assert_eq!(c, t.plate_coat(Material::Panel, Rgba { a: 255, ..t.popup_tint() }, s));
                assert_eq!(Some(c.fill), t.flat_fill(Material::Panel));
                // Flat (since 2026-11-12): one grey, no grain, no rim.
                assert!(c.grain == 0.0 && c.grain == t.flat_grain(Material::Panel), "light {light}: no grain");
                assert_eq!(c.rim.map(|r| r.0.map(|w| w.a)), Some([0; 3]), "no rim at {s}");
            }
            // A Flat the PC forces (Windows' transparency off, the blur
            // off): the same plate, no grain.
            for (sys, blur) in [(SysLook { light, transparency: false, ..Default::default() }, true), (sys, false)] {
                let f = Theme::graphite().resolve_fx(&sys, blur);
                let c = panel_coat(&f, 1.5);
                assert_eq!((c.fill, c.grain), (panel_coat(&t, 1.5).fill, 0.0));
            }
            // A glass (its folder panel is opaque too): one color, no
            // grain, the rim one device pixel.
            let mut raw = Theme::graphite();
            raw.finish = Finish::Glass;
            let g = raw.resolve_fx(&sys, true);
            let c = panel_coat(&g, 1.5);
            assert!(c.fill.0 == c.fill.1 && c.grain == 0.0);
            assert_eq!(c.rim.map(|r| r.1), Some(1.0 / 1.5));
            // High contrast: Window, no grain, no rim (its outline).
            let hc = Theme::graphite().resolve_fx(&SysLook { light, high_contrast: Some(crate::syslook::HighContrast { window: Rgba { r: 0, g: 0, b: 0, a: 255 }, text: Rgba { r: 255, g: 255, b: 255, a: 255 }, highlight: Rgba { r: 255, g: 255, b: 255, a: 255 }, highlight_text: Rgba { r: 0, g: 0, b: 0, a: 255 }, gray_text: crate::syslook::GRAY_TEXT }), ..Default::default() }, true);
            let c = panel_coat(&hc, 1.5);
            assert!(c.grain == 0.0 && c.rim.is_none() && c.fill.0 == c.fill.1);
        }
    }

    #[test]
    fn a_folder_taller_than_its_panel_fades_under_its_name_only_when_scrolled() {
        let g = grid::grid(1280.0, 720.0, grid::LABEL_H);
        // More apps than the panel shows: it scrolls.
        let n = 40;
        let pg = folders::panel_grid(n, &g, 1280.0, 720.0);
        assert!(folders::max_scroll(n, &pg) > 0);
        assert_eq!(folder_bands(n, &pg, 0), Some((false, false)), "at the top: no band");
        assert_eq!(folder_bands(n, &pg, 1), Some((true, false)), "rows above: the band");
        assert_eq!(folder_bands(n, &pg, folders::max_scroll(n, &pg)), Some((true, false)), "no foot: never at the bottom");
        // All its rows shown: no edge at all.
        let few = folders::panel_grid(3, &g, 1280.0, 720.0);
        assert_eq!(folder_bands(3, &few, 0), None);
        // The area: from under the name's row to the panel's bottom, its
        // full width; the band fits in it.
        let (a, p) = (folder_scroll_area(&pg), folders::panel_rect(&pg));
        assert_eq!((a.l, a.r, a.b), (p.l, p.r, p.b));
        assert_eq!(a.t, p.t + folders::PANEL_TITLE);
        assert!(a.h() > size::SCROLL_FADE * 2.0);
        // On whole pixels at 150 %.
        let r = on_px(Rect::new(10.1, 20.2, 300.3, 400.4), 1.5);
        for v in [r.l, r.t, r.r, r.b] {
            assert_eq!((v * 1.5).fract(), 0.0, "{v}");
        }
    }

    #[test]
    fn done_is_a_capsule_a_control_tall() {
        // 12 either side of its label, 28 tall (3.2), at the trailing side
        // of the work area, level with the search field's middle.
        assert_eq!(done_size(40.2), (65.0, 28.0));
        let work = Rect::new(0.0, 40.0, 1920.0, 1040.0);
        let r = done_place(&work, 65.0, false);
        assert_eq!((r.r, r.w(), r.h()), (1920.0 - DONE_SIDE, 65.0, size::CONTROL_H));
        assert_eq!(r.cy(), work.t + grid::FIELD_TOP + grid::FIELD_H / 2.0);
        assert_eq!(done_place(&work, 65.0, true).l, DONE_SIDE, "mirrored");
    }

    #[test]
    fn strokes_lie_inside_their_edge() {
        let r = inner(D2D_RECT_F { left: 0.0, top: 0.0, right: 100.0, bottom: 40.0 }, 12.0, 2.0);
        assert_eq!((r.rect.left, r.rect.top, r.rect.right, r.rect.bottom, r.radiusX), (1.0, 1.0, 99.0, 39.0, 11.0));
        assert_eq!(inner(D2D_RECT_F { left: 0.0, top: 0.0, right: 10.0, bottom: 10.0 }, 0.5, 2.0).radiusX, 0.0);
    }

    #[test]
    fn the_edit_outline_fades_on_from_where_it_shows() {
        // First into edit mode: from nothing.
        assert_eq!(ring_from(true), Some(0.0));
        // Back in while it was still fading out: from where it is (no drop
        // to nothing first).
        assert_eq!(ring_from(false), None);
    }
}

#[cfg(test)]
mod review_focus_tests {
    use super::*;
    use super::super::*;
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_MENU, VK_LWIN};

    #[test]
    fn review_launchpad_modified_navigation() {
        let stage = crate::gfx::promo_kit::stage();
        unsafe { register_class(); }
        let mut p = unsafe { Pad::new(&stage.gfx, &Theme::graphite(), None).unwrap() };
        p.searching = true;
        p.apps = vec![App { name: "Focus test".into(), id: "test".into() }];
        p.found = vec![0];
        let keys = crate::input::sample::Keys::new();
        for rtl in [false, true] {
            p.rtl = rtl;
            for modifier in [VK_CONTROL.0, VK_MENU.0, VK_LWIN.0] {
                for vk in [VK_TAB.0, VK_DOWN.0] {
                    p.pointer_takes_over();
                    p.focus = Focus::Field;
                    keys.set(Some(modifier));
                    p.on_key(vk);
                    assert_eq!(p.focus, Focus::Grid);
                    assert_eq!(p.sel, Some(0));
                    assert!(!p.by_key && !p.field_ring);
                    keys.set(None);
                    p.on_key(VK_UP.0);
                    assert!(p.by_key && p.field_ring);
                }
            }
        }
        unsafe { let _ = DestroyWindow(p.hwnd); }
    }

    #[test]
    fn review_launchpad_all_buttons_hide_rings() {
        pointer_transition(false);
    }

    #[test]
    fn review_done_picture_loses_reader_ring_at_rest() {
        pointer_transition(true);
    }

    fn pointer_transition(done: bool) {
        let stage = crate::gfx::promo_kit::stage();
        unsafe { register_class(); }
        let p = unsafe { Pad::new(&stage.gfx, &Theme::graphite(), None).unwrap() };
        let hwnd = p.hwnd;
        PAD.with(|slot| *slot.borrow_mut() = Some(p));
        // Dispatch actual button messages without moving the pointer first.
        for msg in [WM_LBUTTONDOWN, WM_RBUTTONDOWN, WM_MBUTTONDOWN] {
            with_pad(|p| {
                p.by_key = true;
                p.field_ring = true;
                p.reader_ring = true;
                p.arr.edit = done;
                p.ax_focus = Some("done".into());
                p.arr.done_state = State::Rest;
                if done {
                    p.draw_done();
                    assert!(p.arr.done_ring);
                }
            });
            unsafe { wndproc(hwnd, msg, WPARAM(0), LPARAM(0)); }
            with_pad(|p| {
                assert!(!p.by_key && !p.field_ring && !p.reader_ring);
                if done { assert!(!p.arr.done_ring, "the cached Done picture must lose its ring"); }
                assert_eq!(p.arr.done_state, State::Rest);
            });
        }
        let p = PAD.with(|slot| slot.borrow_mut().take().unwrap());
        unsafe { let _ = DestroyWindow(p.hwnd); }
    }
}
