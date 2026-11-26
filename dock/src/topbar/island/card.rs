//! The Live Island's card: what the pill holds, in full. The pill and the
//! card are one thing (design system v2.1, 7.2 rule 4): the card's window
//! opens over the pill and one plate (morph.rs) grows out of it, in place,
//! from the pill's top edge down (up over a bar at the bottom); closing,
//! it goes back the same way, and turns round where it is if it is opened
//! again meanwhile. Its content is drawn into one D2D surface under that
//! plate's clip; the progress rows' app icons are the dock's own brushes
//! on visuals above it; the hover fill is a visual under it (it fades,
//! and answers a press with its size).
//!
//! Not modal: the bar keeps working while it is out. Opened by hovering it
//! never takes the keyboard (it goes when the pointer leaves the card);
//! opened by a click or a screen reader ("pinned") it takes the keyboard
//! and the mouse (Tab / arrows between its buttons, Enter, Esc; a click
//! outside closes it). The island (mod.rs) owns the state: the card only
//! reports (WM_ISLAND_ACT to the bar) and is closed by it.
//!
//! What is playing has a position bar (when the app gives a timeline) and
//! the app's own volume (its audio session, audio::session). The position
//! is moved on by the card's clock between the app's reports: a tick
//! re-armed for the next whole second, only while the card is out and the
//! track plays. The volume is read when the card opens (or the app
//! changes) and set as the slider moves.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::core::{w, Interface, Result, BOOL, HSTRING};
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{
    CompositionBrush, CompositionColorBrush, CompositionRoundedRectangleGeometry, CompositionStretch, CompositionStrokeCap, Compositor, ContainerVisual, SpriteVisual, Visual,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{
    ID2D1DeviceContext, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_BRUSH_PROPERTIES1, D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1,
    D2D1_BRUSH_PROPERTIES, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_EXTEND_MODE_CLAMP, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
    D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{
    IDWriteTextLayout, DWRITE_FONT_FEATURE, DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_METRICS,
    DWRITE_TEXT_RANGE,
};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_HOSTBACKDROPBRUSH};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, GetKeyState, ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_DOWN, VK_ESCAPE, VK_LEFT,
    VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_numerics::{Matrix3x2, Vector2, Vector3};

use super::model::{self, Activity, Power};
use super::morph;
use super::{Art, ACT_DISMISS, ACT_LEAVE, ACT_NEXT, ACT_PIN, ACT_PIP, ACT_PREV, ACT_SEEK, ACT_SHOT, ACT_TIMER, ACT_TOGGLE, WM_ISLAND_ACT};
use crate::a11y::{self, Act, Node, Range, Role, Tree};
use crate::audio::session;
use crate::gfx::Gfx;
use crate::glyphs::Glyph;
use crate::i18n::t;
use crate::theme::metrics::{size, space};
use crate::theme::{ramp, Level, Role as Kind, State, Theme};

const W: f32 = 340.0;
/// The two lines everything stands on (design system v2.1, 3.3): an
/// activity's rows (their hover fill) reach the vessel line, PANEL_PAD in
/// from the card's edge; what is in them (art, glyphs, times, bars) starts
/// at the content line, CARD_PAD further in. Mirrored at the other edge.
const PAD: f32 = size::PANEL_PAD;
const LINE: f32 = PAD + size::CARD_PAD;
/// Between activities: a slot with a hairline in its middle, from content
/// line to content line.
const SEP: f32 = size::SEP_H;
/// An activity's row: two lines (its name over a bar or a note) beside a
/// 28 box holding its icon or a 24 glyph.
const ROW: f32 = size::ROW_2XL;
/// A one-line row (who uses the camera or the microphone).
const ONE: f32 = size::ROW_M;
const ICON: f32 = 28.0;
/// Where a two-line row's words start: the box and 12 after it.
const TEXT_X: f32 = LINE + ICON + space::S4;
/// What is playing (top to bottom): CARD_PAD, the art's line (title and
/// artist centered beside it, the buttons at its end), 8, the times and
/// the position bar, 8, the volume, CARD_PAD.
const ART: f32 = size::ART;
const TIME_H: f32 = ramp::FOOTNOTE.line;
const VOL_H: f32 = 20.0;
const GAP: f32 = space::S3;
/// Every bar is one thickness and a capsule; a slider's knob is 16 with
/// E1 and the contact shadow (gfx::parts::knob).
const TRACK_H: f32 = size::TRACK_H;
const KNOB_R: f32 = size::KNOB / 2.0;
/// How tall a slider is to the pointer (the track itself is thin).
const GRIP_H: f32 = VOL_H;
/// Glyph buttons: a 28 capsule holding a 16 glyph, 4 apart.
const BTN: f32 = size::CONTROL_H;
const BTN_GAP: f32 = space::S2;
const GLYPH: f32 = size::ICON_S;
const GLYPH_L: f32 = size::ICON_L;
/// The position bar's times.
const TIME: f32 = ramp::FOOTNOTE.size;
/// The volume's step for a key or a wheel notch (percent).
const VOL_STEP: i32 = 5;
/// One wheel notch (WHEEL_DELTA).
const WHEEL_NOTCH: i32 = 120;
/// A seek just asked holds the bar where it was sent this long, or until
/// the app's report agrees (no jump back to the old place meanwhile).
const SEEK_HOLD: Duration = Duration::from_millis(2500);
/// The position's tick (re-armed for the next whole second).
const TICK_ID: usize = 1;
/// Posted to the card by the audio worker: the media app's volume.
const WM_CARD_VOLUME: u32 = WM_APP + 93;
/// Room around the card for its shadow (the window is this much larger).
const SHADOW_M: f32 = crate::theme::metrics::SHADOW_M;
const TEXT: f32 = ramp::BODY.size;
const SMALL: f32 = ramp::FOOTNOTE.size;
const WM_MOUSELEAVE_MSG: u32 = 0x02A3;
/// The bar's own pill and the plate over it change places this long apart
/// (ms): the one that takes over is on screen before the other goes, so
/// no frame shows neither (two windows are not shown in one commit).
const HANDOVER_MS: u32 = 48;

const G_PREV: Glyph = Glyph::Prev;
const G_NEXT: Glyph = Glyph::Next;
const G_PLAY: Glyph = Glyph::Play;
const G_PAUSE: Glyph = Glyph::Pause;
const G_MUSIC: Glyph = Glyph::Music;
const G_BOLT: Glyph = Glyph::Bolt;
const G_BATTERY_LOW: Glyph = Glyph::Battery(0);
const G_TIMER: Glyph = Glyph::Timer;
const G_SHOT: Glyph = Glyph::Camera;
const G_STOP: Glyph = Glyph::Stop;
/// The screenshot's buttons: save, copy, edit.
const SHOT_BUTTONS: [(u8, Glyph, &str); 3] = [(1, Glyph::Save, "island.shot.save"), (2, Glyph::Copy, "island.shot.copy"), (4, Glyph::Edit, "island.shot.edit")];
/// The screenshot's picture: this tall in its row.
const SHOT_H: f32 = 32.0;
/// The focus timer's note line (Do not disturb is not available).
const NOTE_H: f32 = ramp::FOOTNOTE.line;
/// The focus timer's buttons: 1 pause / resume, 2 skip, 3 "+5 min", 4 stop.
const TIMER_BUTTONS: [u8; 4] = [1, 2, 3, 4];
/// What the card shows (a snapshot from the island).
pub(super) struct Data {
    pub live: Vec<Activity>,
    pub icons: HashMap<String, CompositionBrush>,
    pub art: Option<Arc<Art>>,
    /// The playing app has a window to show in a PiP (its button shows).
    pub pip: bool,
}

/// The bar's pill, for the plate that stands in for it (morph.rs).
pub(super) struct Pill {
    /// The pill on screen (px), at scale 1.
    pub rect: RECT,
    /// Its scale now about its middle (a hover, a press on its way).
    pub scale: f32,
    /// Its content, in its own DIP (x 0 its middle, y 0 its top).
    pub parts: Vec<Part>,
    /// Everything of it in the bar's window: hidden while the plate is
    /// out.
    pub shell: Visual,
}

/// One part of the pill's content.
pub(super) enum Part {
    /// A drawn surface (the lead, the title, a time, a dot), its corners.
    Picture { brush: CompositionBrush, x: f32, y: f32, w: f32, h: f32, radius: f32 },
    /// A plain rectangle (a music bar).
    Fill { color: crate::theme::Rgba, x: f32, y: f32, w: f32, h: f32 },
    /// The focus timer's ring: its track and the share still to go.
    Ring { x: f32, y: f32, side: f32, stroke: f32, track: crate::theme::Rgba, arc: crate::theme::Rgba, share: f32 },
}

pub(super) struct Request {
    pub gfx: Rc<Gfx>,
    pub theme: Theme,
    pub scale: f32,
    pub pill: Pill,
    pub bar: HWND,
    pub data: Data,
    pub pinned: bool,
}

/// One row of the card.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Row {
    /// Activity index, job index.
    Job(usize, usize),
    Media(usize),
    Battery(usize),
    Timer(usize),
    /// Activity index, app index (model::by_app).
    Privacy(usize, usize),
    Shot(usize),
}

/// What a click or Enter does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Target {
    /// Bring the job's app to the front.
    Job(usize, usize),
    Prev,
    Toggle,
    Next,
    /// What plays, in a picture-in-picture window (crate::pip).
    Pip,
    /// The screenshot: 0 its picture (dragged out), 1..=4 its buttons.
    Shot(u8),
    /// The focus timer's buttons (TIMER_BUTTONS).
    Timer(u8),
    /// What is playing: its position bar and its app's volume (sliders).
    Seek,
    Volume,
}

impl Target {
    fn slider(self) -> bool {
        matches!(self, Target::Seek | Target::Volume)
    }
}

/// The track has a position bar.
fn has_bar(tr: &model::Track) -> bool {
    tr.timeline.is_some_and(|t| t.shown())
}

/// What is playing, `bar`: with its position bar; `volume`: with its
/// app's volume under it.
fn media_height(bar: bool, volume: bool) -> f32 {
    size::CARD_PAD + ART + if bar { GAP + TIME_H } else { 0.0 } + if volume { GAP + VOL_H } else { 0.0 } + size::CARD_PAD
}

/// The rows top to bottom: (row, y, height), the separators' y, and the
/// card's height (DIP). `volume`: what is playing has its volume row (its
/// app's volume is known, or not answered yet). Pure.
pub(super) fn layout(live: &[Activity], volume: bool) -> (Vec<(Row, f32, f32)>, Vec<f32>, f32) {
    let mut rows = Vec::new();
    let mut seps = Vec::new();
    let mut y = PAD;
    for (ai, a) in live.iter().enumerate() {
        if ai > 0 {
            seps.push(y + SEP / 2.0);
            y += SEP;
        }
        let mut push = |r: Row, h: f32| {
            rows.push((r, y, h));
            y += h;
        };
        match a {
            Activity::Progress(jobs) => (0..jobs.len()).for_each(|j| push(Row::Job(ai, j), ROW)),
            Activity::Media(tr) => push(Row::Media(ai), media_height(has_bar(tr), volume)),
            Activity::Battery(_) => push(Row::Battery(ai), ROW),
            Activity::Timer(tm) => push(Row::Timer(ai), timer_height(tm)),
            Activity::Privacy(u) => (0..model::by_app(u).len()).for_each(|g| push(Row::Privacy(ai, g), ONE)),
            Activity::Screenshot(_) => push(Row::Shot(ai), ROW),
        }
    }
    (rows, seps, y + PAD)
}

/// The focus timer's row: its two lines, and while it runs its buttons
/// under them (and a note when Do not disturb is not available).
fn timer_height(tm: &model::Timer) -> f32 {
    match &tm.focus {
        Some(v) if v.phase < 2 => ROW + BTN + if v.no_dnd { space::S2 + NOTE_H } else { 0.0 } + space::S2,
        _ => ROW,
    }
}

/// Pure: a two-line row at `y`, a name over a bar: (the name's middle,
/// the bar's top). The lines and the 8 between them centered in the row.
fn bar_lines(y: f32) -> (f32, f32) {
    let top = y + (ROW - (ramp::BODY.line + GAP + TRACK_H)) / 2.0;
    (top + ramp::BODY.line / 2.0, top + ramp::BODY.line + GAP)
}

/// Pure: a two-line row at `y`, a name over a note: their middles.
fn note_lines(y: f32) -> (f32, f32) {
    let top = y + (ROW - (ramp::BODY.line + ramp::FOOTNOTE.line)) / 2.0;
    (top + ramp::BODY.line / 2.0, top + ramp::BODY.line + ramp::FOOTNOTE.line / 2.0)
}

/// Pure: `n` glyph buttons ending at the content line's mirror: each
/// one's left edge.
fn buttons_from_end(n: usize) -> Vec<f32> {
    let first = W - LINE - n as f32 * BTN - n.saturating_sub(1) as f32 * BTN_GAP;
    (0..n).map(|k| first + k as f32 * (BTN + BTN_GAP)).collect()
}

/// The focus timer's buttons' words: pause or resume, skip, "+5 min", stop.
fn timer_button_text(k: u8, running: bool) -> &'static str {
    t(match k {
        1 if running => "timer.pause",
        1 => "timer.resume",
        2 => "timer.skip",
        3 => "timer.plus5",
        _ => "timer.stop",
    })
}

/// The keyboard's order of the card's targets (`volume`: the playing app's
/// volume is known, its slider shows; `pip`: its window can be shown in a
/// PiP).
pub(super) fn targets(live: &[Activity], volume: bool, pip: bool) -> Vec<Target> {
    let mut out = Vec::new();
    for (ai, a) in live.iter().enumerate() {
        match a {
            Activity::Progress(jobs) => out.extend((0..jobs.len()).filter(|&j| jobs[j].hwnd != 0).map(|j| Target::Job(ai, j))),
            Activity::Media(tr) => {
                // As they stand: the buttons at the end of the art's line
                // (the PiP first), the position bar, the volume.
                if pip {
                    out.push(Target::Pip);
                }
                if tr.can_prev {
                    out.push(Target::Prev);
                }
                if tr.can_toggle {
                    out.push(Target::Toggle);
                }
                if tr.can_next {
                    out.push(Target::Next);
                }
                // A bar the app will not move is only read.
                if tr.timeline.is_some_and(|t| t.can_seek()) {
                    out.push(Target::Seek);
                }
                if volume {
                    out.push(Target::Volume);
                }
            }
            Activity::Screenshot(_) => out.extend(SHOT_BUTTONS.iter().map(|b| Target::Shot(b.0))),
            Activity::Timer(tm) if tm.focus.as_ref().is_some_and(|v| v.phase < 2) => out.extend(TIMER_BUTTONS.iter().map(|k| Target::Timer(*k))),
            _ => {}
        }
    }
    out
}

/// The next target from `cur` in `dir` (wraps; none yet: the first or last).
pub(super) fn step_focus(order: &[Target], cur: Option<Target>, dir: i32) -> Option<Target> {
    if order.is_empty() {
        return None;
    }
    let n = order.len() as i32;
    let i = match cur.and_then(|c| order.iter().position(|t| *t == c)) {
        Some(i) => (i as i32 + dir).rem_euclid(n),
        None => if dir >= 0 { 0 } else { n - 1 },
    };
    Some(order[i as usize])
}

fn key_of(t: Target) -> String {
    match t {
        Target::Job(a, j) => format!("job:{a}:{j}"),
        Target::Prev => "prev".into(),
        Target::Toggle => "toggle".into(),
        Target::Next => "next".into(),
        Target::Pip => "pip".into(),
        Target::Shot(k) => format!("shot:{k}"),
        Target::Timer(k) => format!("timer:{k}"),
        Target::Seek => "seek".into(),
        Target::Volume => "volume".into(),
    }
}

fn rgba(c: crate::theme::Rgba) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.r as f32 / 255.0, g: c.g as f32 / 255.0, b: c.b as f32 / 255.0, a: c.a as f32 / 255.0 }
}

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

fn inside(r: &D2D_RECT_F, x: f32, y: f32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

/// Where the card stands (7.4): on the pill. Its top edge is the pill's
/// (its bottom edge over a bar at the bottom, `up`: it grows upward), its
/// middle the pill's, kept EDGE_GAP inside the work area's sides. `pill`:
/// the pill on screen (px). Returns the rect (px) and the shrink (a short
/// screen: to the room there is from the pill to the far edge).
fn place(pill: RECT, work: RECT, scale: f32, h: f32, up: bool) -> (RECT, f32) {
    let gap = (size::EDGE_GAP * scale).round() as i32;
    let room_px = (if up { pill.bottom - work.top } else { work.bottom - pill.top } - gap).max(1);
    let room = room_px as f32;
    let full = (h * scale).ceil();
    let fit = if full > room { room / full } else { 1.0 };
    let ds = scale * fit;
    let w_px = (W * ds).ceil() as i32;
    let h_px = ((h * ds).ceil() as i32).min(room_px);
    let cx = (pill.left + pill.right) / 2;
    let x = (cx - w_px / 2).min(work.right - gap - w_px).max(work.left + gap);
    let y = if up { pill.bottom - h_px } else { pill.top };
    (RECT { left: x, top: y, right: x + w_px, bottom: y + h_px }, fit)
}

/// Pure: the pill (on screen at `pill` px, scaled `k` about its middle)
/// in the coordinates of a card at `card` px drawn `ds` px to the DIP: the
/// plate's shape at p = 0, a capsule.
pub(super) fn pill_shape(pill: RECT, k: f32, card: RECT, ds: f32) -> morph::Shape {
    let ds = ds.max(0.01);
    let (w, h) = ((pill.right - pill.left) as f32, (pill.bottom - pill.top) as f32);
    let (cx, cy) = (pill.left as f32 + w / 2.0 - card.left as f32, pill.top as f32 + h / 2.0 - card.top as f32);
    let (w, h) = (w * k, h * k);
    morph::Shape { x: (cx - w / 2.0) / ds, y: (cy - h / 2.0) / ds, w: w / ds, h: h / ds, r: h / ds / 2.0 }
}

/// Pure: the window's margin around the card (px): room for E3, and for a
/// pill that reaches past the card (a card the screen's edge pushed
/// aside) with its own shadow.
fn margin_px(pill: RECT, card: RECT, scale: f32) -> i32 {
    let over = (card.left - pill.left).max(pill.right - card.right).max(card.top - pill.top).max(pill.bottom - card.bottom).max(0);
    let own = (crate::theme::Elevation::E1.reach() * scale).ceil() as i32 + (pill.right - pill.left) / 20;
    ((SHADOW_M * scale).ceil() as i32).max(over + own)
}

/// The card's content takes the pointer once this much of it shows
/// (morph::card_alpha, p = 0.675): nothing is pressed before it is seen.
const LIVE_ALPHA: f32 = 0.5;

/// Where a pointer is on the card as it is drawn now.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Spot {
    /// Off the plate (and off the pill it is still growing out of).
    Outside,
    /// On the pill the plate is still growing out of.
    Pill,
    /// On the plate: the target there (None: none, or the content does
    /// not take the pointer yet) and the point in the content's own DIP.
    Card(Option<Target>, f32, f32),
}

/// Pure: where card-window DIP (`x`, `y`) is on the card drawn as `d`
/// (morph::Plate::drawn): the same clip and the same place of the content
/// the compositor shows, so nothing unseen is pressed and what is seen is
/// what is pressed. `opening`: still on its way out of the pill. `hit`:
/// the target at a point of the content (its own DIP, as painted).
fn spot(d: &morph::Drawn, opening: bool, x: f32, y: f32, hit: impl Fn(f32, f32) -> Option<Target>) -> Spot {
    let s = d.pill;
    if opening && x >= s.x && x < s.x + s.w && y >= s.y && y < s.y + s.h {
        return Spot::Pill;
    }
    if !morph::on_shape(d.plate, x, y) {
        return Spot::Outside;
    }
    let (cx, cy) = d.to_content(x, y);
    let live = morph::card_alpha(d.p) >= LIVE_ALPHA;
    Spot::Card(if live { hit(cx, cy) } else { None }, cx, cy)
}

/// What a press does to the card.
#[derive(Clone, Copy, Debug, PartialEq)]
enum PressDo {
    /// The island closes it (ACT_DISMISS: back into the pill).
    Dismiss,
    /// The island keeps it (ACT_PIN: a card out by hovering stays).
    Pin,
    /// On the card: what is under it (a left press).
    Press(Option<Target>),
    Nothing,
}

/// Pure: what a press at `spot` does to a card `pinned` (out by a click)
/// or not (out by hovering); `left`: the left button.
fn press_do(spot: Spot, pinned: bool, left: bool) -> PressDo {
    match spot {
        // (The pill included once the plate stands, held by the capture.)
        Spot::Outside => PressDo::Dismiss,
        Spot::Pill | Spot::Card(..) if !left => PressDo::Nothing,
        // Pressed again while it opens (7.2): a card out by a click goes
        // back, the morph turning round where it is; one out by hovering
        // stays, as a click on it does.
        Spot::Pill if pinned => PressDo::Dismiss,
        Spot::Pill => PressDo::Pin,
        Spot::Card(t, ..) => PressDo::Press(t),
    }
}

/// The hover fill under the pointer's target: a visual of its own under
/// the card's picture, so it fades (on FADE_IN_MS, off FADE_OUT_MS) and
/// answers a press with its size (crate::motion::press) while the picture
/// itself is drawn once.
struct Wash {
    v: SpriteVisual,
    geom: CompositionRoundedRectangleGeometry,
    brush: CompositionColorBrush,
    on: Option<WashOn>,
}

/// What a wash shows: its target, pressed or not, and the target's shape
/// as last drawn (box, corner radius).
#[derive(Clone, Copy, Debug, PartialEq)]
struct WashOn {
    t: Target,
    down: bool,
    r: D2D_RECT_F,
    rad: f32,
}

/// What the wash has to do to show `want` (sync_wash).
#[derive(Clone, Copy, Debug, PartialEq)]
enum WashStep {
    /// It shows it already.
    Stay,
    /// Another target (or none): the one shown fades, the other wash
    /// comes on.
    Other,
    /// The same target: pressed or let go, and/or drawn somewhere else
    /// (the rows above it changed height): its fill and press, its place.
    Same { press: bool, moved: bool },
}

/// Pure: the wash's step from `now` to `want`.
fn wash_step(now: Option<WashOn>, want: Option<WashOn>) -> WashStep {
    match (now, want) {
        (None, None) => WashStep::Stay,
        (Some(a), Some(b)) if a.t == b.t => {
            let (press, moved) = (a.down != b.down, a.r != b.r || a.rad != b.rad);
            if press || moved {
                WashStep::Same { press, moved }
            } else {
                WashStep::Stay
            }
        }
        _ => WashStep::Other,
    }
}

impl Wash {
    fn new(c: &Compositor, parent: &ContainerVisual) -> Result<Wash> {
        let v = c.CreateSpriteVisual()?;
        let geom = c.CreateRoundedRectangleGeometry()?;
        let brush = c.CreateColorBrush()?;
        v.SetBrush(&brush)?;
        v.SetClip(&c.CreateGeometricClipWithGeometry(&geom)?)?;
        v.SetOpacity(0.0)?;
        parent.Children()?.InsertAtBottom(&v)?;
        Ok(Wash { v, geom, brush, on: None })
    }

    /// On its target's shape (a press's scale keeps to its middle).
    fn place(&self, r: D2D_RECT_F, rad: f32) {
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let _ = self.v.SetOffset(Vector3 { X: r.left, Y: r.top, Z: 0.0 });
        let _ = self.v.SetSize(Vector2 { X: w, Y: h });
        let _ = self.v.SetCenterPoint(Vector3 { X: w / 2.0, Y: h / 2.0, Z: 0.0 });
        let _ = self.geom.SetSize(Vector2 { X: w, Y: h });
        let _ = self.geom.SetCornerRadius(Vector2 { X: rad, Y: rad });
    }
}

/// What a paint leaves besides the picture: the targets' boxes for the
/// pointer, their shapes for the wash, the sliders' tracks, and where the
/// dock's icons go.
#[derive(Default)]
struct Painted {
    hits: Vec<(D2D_RECT_F, Target)>,
    shapes: Vec<(Target, D2D_RECT_F, f32)>,
    sliders: Vec<(Target, f32, f32)>,
    icons: Vec<(String, D2D_RECT_F)>,
}

struct Card {
    hwnd: HWND,
    bar: HWND,
    gfx: Rc<Gfx>,
    theme: Theme,
    scale: f32,
    fit: f32,
    _target: DesktopWindowTarget,
    outer: ContainerVisual,
    /// Holds the plate; faded as a whole with reduced motion and when the
    /// card is closed without going back into the pill.
    anim: ContainerVisual,
    /// The pill and the card as one thing (morph.rs).
    plate: morph::Plate,
    /// The card's picture: two, to cross from one set of rows to another
    /// (7.4: content changing in place); the progress rows' icons are
    /// children of each (`icons`: each one's own).
    content: [SpriteVisual; 2],
    cur: usize,
    /// The rows last drawn: another set crosses in.
    rows: Vec<(Row, u32)>,
    drawn: bool,
    washes: [Wash; 2],
    wash: usize,
    /// The icons on each picture: the one fading out keeps its own until
    /// it has.
    icons: [Vec<SpriteVisual>; 2],
    rect: RECT,
    /// The pill on screen (px, at scale 1) and its scale when it was
    /// taken.
    pill: RECT,
    /// The bar's own pill, hidden while the plate is out.
    shell: Visual,
    /// The window's margin around `rect` (px).
    m_px: i32,
    work: RECT,
    /// Over a bar at the bottom: the card grows upward.
    up: bool,
    height: f32,
    data: Data,
    pinned: bool,
    /// On its way back into the pill: it takes no input, and `open` may
    /// turn it round.
    closing: bool,
    /// One-shots (crate::motion::after): the plate has arrived; the bar's
    /// pill goes once the plate is over it; the window shrinks once the
    /// plate has.
    settle: Option<usize>,
    hide: Option<usize>,
    shrink: Option<usize>,
    hits: Vec<(D2D_RECT_F, Target)>,
    /// The targets' shapes as drawn: (target, box, corner radius).
    shapes: Vec<(Target, D2D_RECT_F, f32)>,
    hover: Option<Target>,
    pressed: Option<Target>,
    focus: Option<Target>,
    keyboard: bool,
    tracking: bool,
    rtl: bool,
    /// Where the left button went down (client px), for a drag.
    press_at: Option<POINT>,
    /// The screenshot is being dragged out (its modal loop holds the
    /// mouse: leaving, capture and focus changes are not a dismissal).
    dragging: bool,
    /// The slider the left button holds (leaving is not a dismissal).
    sliding: Option<Target>,
    /// Where the position bar is dragged to: (the track's gen, ms), sought
    /// on release if it is still that track.
    drag_pos: Option<(u32, i64)>,
    /// A seek just sent: (the track's gen, position, when), shown until
    /// the app agrees, refuses, the track changes or SEEK_HOLD passes.
    seek_hold: Option<(u32, i64, Instant)>,
    /// The track's count of refused seeks last seen (a new one drops the
    /// hold).
    seen_refused: Option<u32>,
    /// Volume answers to requests older than this (the newest change, or
    /// the first read) are not shown.
    vol_floor: u64,
    /// The playing app's volume: (its app id, percent, muted). None:
    /// unknown yet, or it has no audio session (no slider).
    volume: Option<(String, u8, bool)>,
    /// The app whose volume was asked for last ("" = none), and whether
    /// it answered (until then its row is kept: the card does not grow a
    /// moment after it opens).
    volume_app: String,
    vol_answered: bool,
    /// The sliders' tracks as drawn: (target, x0, x1).
    sliders: Vec<(Target, f32, f32)>,
    /// The position's tick is set.
    ticking: bool,
    /// Wheel deltas short of a notch (touchpads send fractions).
    wheel_acc: i32,
}

thread_local! {
    static CARD: RefCell<Option<Card>> = const { RefCell::new(None) };
}

fn with_card<R>(f: impl FnOnce(&mut Card) -> R) -> Option<R> {
    CARD.with(|c| c.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

/// The card out now (or on its way back into the pill) was opened by bar
/// `bar`.
pub(super) fn opened_by(bar: HWND) -> bool {
    with_card(|c| c.bar == bar).unwrap_or(false)
}

/// The card is out (one going back into the pill is not).
pub(super) fn is_open() -> bool {
    CARD.with(|c| c.try_borrow().map(|g| g.as_ref().is_some_and(|c| !c.closing)).unwrap_or(true))
}

/// The pointer is over the card now.
pub(super) fn pointer_inside() -> bool {
    let mut pt = POINT::default();
    if unsafe { GetCursorPos(&mut pt) }.is_err() {
        return false;
    }
    with_card(|c| !c.closing && (c.dragging || c.sliding.is_some() || c.spot_on_screen(pt) != Spot::Outside)).unwrap_or(false)
}

/// Opens the card out of the pill (7.4): a card still on its way back
/// into this bar's pill turns round where it is; else any other goes and
/// a new one grows from the pill. False if it could not.
pub(super) fn open(req: Request) -> bool {
    // Another popup holds the mouse: not now.
    if crate::popup::is_tracking() || super::super::control::is_open() || super::super::stats::is_open() {
        close();
        return false;
    }
    let pinned = req.pinned;
    let mut req = Some(req);
    let turned = with_card(|c| {
        let mine = c.closing && req.as_ref().is_some_and(|r| r.bar == c.bar) && crate::motion::full();
        if mine {
            c.turn_round(req.take().unwrap());
        }
        mine.then_some(c.hwnd)
    })
    .flatten();
    if let Some(hwnd) = turned {
        a11y::menu_event(hwnd, true);
        if pinned {
            pin();
        }
        return true;
    }
    let Some(req) = req else { return false };
    close();
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: w!("NopBarIsland"),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        // Fails harmlessly once registered.
        RegisterClassExW(&wc);
        let card = match Card::new(req) {
            Ok(c) => c,
            Err(e) => {
                crate::log!("island: cannot open the card: {e}");
                return false;
            }
        };
        let hwnd = card.hwnd;
        CARD.with(|c| *c.borrow_mut() = Some(card));
        with_card(|c| {
            c.ask_volume();
            c.settle_hold();
            c.relayout();
            c.redraw();
            c.sync_tick();
        });
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        with_card(|c| c.open_motion());
        a11y::menu_event(hwnd, true);
        if pinned {
            pin();
        }
    }
    true
}

/// The card takes the keyboard and the mouse (a click or a screen reader).
pub(super) fn pin() {
    let h = with_card(|c| {
        c.pinned = true;
        c.hwnd
    });
    let Some(h) = h else { return };
    unsafe {
        let _ = SetForegroundWindow(h);
        let _ = SetFocus(Some(h));
        SetCapture(h);
    }
    with_card(|c| {
        // The first button holds the focus (Enter, a screen reader), but its
        // ring shows only once a key moves it: a card opened with the mouse
        // drew a hard ring around play/pause as if Tab had been pressed.
        c.keyboard = false;
        if c.focus.is_none() {
            c.focus = step_focus(&c.order(), None, 1);
        }
        c.redraw();
        c.publish();
    });
}

/// New things to show (and the pill as it is now: where the plate goes
/// back to).
pub(super) fn update(data: Data, pill: Option<Pill>) {
    with_card(|c| {
        if let Some(p) = pill {
            c.take_pill(p, false);
        }
        if c.closing {
            return Released::default();
        }
        c.data = data;
        c.ask_volume();
        let released = c.forget_gone();
        c.settle_hold();
        c.relayout();
        c.redraw();
        c.rehover();
        c.publish();
        c.sync_tick();
        released
    })
    .map(Released::run);
}

/// The card goes back into the pill `pill` (7.4: the same GLIDE,
/// backwards; a card still opening turns round where it is). With reduced
/// motion nothing changes shape: the card fades out as the pill fades in,
/// in place.
pub(super) fn close_into(pill: Pill) {
    if !crate::motion::full() {
        close();
        return;
    }
    let (folded, released) = with_card(|c| {
        if c.closing {
            return (true, Released::default());
        }
        let released = c.let_go(false);
        (c.fold(pill), released)
    })
    .unwrap_or((true, Released::default()));
    released.run();
    if !folded {
        close();
    }
}

/// Closes the card where it stands (from the island only): a quick fade,
/// the bar's own pill back at once (in the same short fade with reduced
/// motion). Nothing is left to go back into, or to turn round.
pub(super) fn close() {
    let card = CARD.with(|c| c.try_borrow_mut().ok().and_then(|mut g| g.take()));
    if let Some(mut c) = card {
        c.cancel_timers();
        c.show_pill(if crate::motion::full() { 0 } else { crate::motion::REDUCED_MS });
        unsafe {
            if GetCapture() == c.hwnd {
                let _ = ReleaseCapture();
            }
            if !c.closing {
                a11y::menu_event(c.hwnd, false);
            }
            // A quick fade, then the window goes (crate::motion).
            let (hwnd, comp, anim) = (c.hwnd, c.gfx.compositor.clone(), c.anim.clone());
            crate::motion::retire(hwnd, &comp, &anim, Box::new(c));
        }
    }
}

/// The plate has arrived (the settle one-shot): the card stands exactly;
/// one that was going back is the pill again, and goes.
fn arrived() {
    let back = with_card(|c| c.arrived()).unwrap_or(false);
    if !back {
        return;
    }
    let card = CARD.with(|c| c.try_borrow_mut().ok().and_then(|mut g| g.take()));
    if let Some(mut c) = card {
        c.cancel_timers();
        // The bar's own pill first; the plate over it goes once that is
        // on screen.
        c.show_pill(0);
        crate::motion::after(
            HANDOVER_MS,
            Box::new(move || unsafe {
                let _ = DestroyWindow(c.hwnd);
                drop(c);
            }),
        );
    }
}

/// What letting go of a slider leaves to do outside the card's borrow
/// (Card::let_go).
#[derive(Default)]
#[must_use]
struct Released {
    hwnd: Option<HWND>,
    /// A hover card releases the capture it took for the slider.
    capture: bool,
    /// The pointer is outside a hover card: the island checks leaving.
    leave: Option<HWND>,
}

impl Released {
    fn run(self) {
        let Some(h) = self.hwnd else { return };
        unsafe {
            if self.capture && GetCapture() == h {
                let _ = ReleaseCapture();
            }
        }
        if let Some(b) = self.leave {
            tell(b, ACT_LEAVE);
        }
    }
}

/// The card's next tick (ms): the time played's next change while it
/// plays (`play`), or a seek's hold running out (`hold`: what is left of
/// it), whichever comes first. Never 0 (a busy loop).
fn next_tick(play: Option<u32>, hold: Option<Duration>) -> Option<u32> {
    let hold = hold.map(|d| (d.as_millis().min(u32::MAX as u128) as u32).saturating_add(15));
    let play = play.map(|ms| ms + 15);
    match (play, hold) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
    .map(|ms| ms.max(16))
}

/// The position bar's RangeValue: seconds from the track's start
/// (fractions kept), over where a seek may go, the position kept inside
/// it; a bar that takes no seek spans the whole track (read only).
fn seek_uia(tl: &model::Timeline, pos: i64) -> Range {
    let (lo, hi) = tl.seek_range().unwrap_or((tl.start_ms, tl.end_ms));
    let s = |ms: i64| model::secs(ms - tl.start_ms);
    Range { value: s(pos.clamp(lo, hi)), min: s(lo), max: s(hi), step: model::secs(model::SEEK_STEP_MS) }
}

/// A volume answer is shown if it is for the app shown and answers a
/// request not older than the newest change (`floor`).
fn volume_answer_counts(r: &session::AppVolume, app: &str, floor: u64) -> bool {
    !app.is_empty() && r.app == app && r.seq >= floor
}

/// Tells the island (the bar), later: never inside its borrow.
fn tell(bar: HWND, act: usize) {
    unsafe {
        let _ = PostMessageW(Some(bar), WM_ISLAND_ACT, WPARAM(act), LPARAM(0));
    }
}

/// The pill's parts as visuals under `ghost` (the plate's copy of the
/// pill's content; `k`: its scale, a card shrunk to a short screen).
fn fill_ghost(c: &Compositor, ghost: &ContainerVisual, parts: &[Part], k: f32) -> Result<()> {
    let kids = ghost.Children()?;
    kids.RemoveAll()?;
    ghost.SetScale(Vector3 { X: k, Y: k, Z: 1.0 })?;
    let col = |c: crate::theme::Rgba| windows::UI::Color { A: c.a, R: c.r, G: c.g, B: c.b };
    for p in parts {
        match p {
            Part::Picture { brush, x, y, w, h, radius } => {
                let v = c.CreateSpriteVisual()?;
                v.SetBrush(brush)?;
                v.SetOffset(Vector3 { X: *x, Y: *y, Z: 0.0 })?;
                v.SetSize(Vector2 { X: *w, Y: *h })?;
                if *radius > 0.0 {
                    let g = c.CreateRoundedRectangleGeometry()?;
                    g.SetSize(Vector2 { X: *w, Y: *h })?;
                    g.SetCornerRadius(Vector2 { X: *radius, Y: *radius })?;
                    v.SetClip(&c.CreateGeometricClipWithGeometry(&g)?)?;
                }
                kids.InsertAtTop(&v)?;
            }
            Part::Fill { color, x, y, w, h } => {
                let v = c.CreateSpriteVisual()?;
                v.SetBrush(&c.CreateColorBrushWithColor(col(*color))?)?;
                v.SetOffset(Vector3 { X: *x, Y: *y, Z: 0.0 })?;
                v.SetSize(Vector2 { X: *w, Y: *h })?;
                kids.InsertAtTop(&v)?;
            }
            Part::Ring { x, y, side, stroke, track, arc, share } => {
                let v = c.CreateShapeVisual()?;
                v.SetOffset(Vector3 { X: *x, Y: *y, Z: 0.0 })?;
                v.SetSize(Vector2 { X: *side, Y: *side })?;
                for (color, trim) in [(track, 1.0), (arc, share.clamp(0.0, 1.0))] {
                    let g = c.CreateEllipseGeometry()?;
                    g.SetCenter(Vector2 { X: side / 2.0, Y: side / 2.0 })?;
                    g.SetRadius(Vector2 { X: side / 2.0 - 1.2, Y: side / 2.0 - 1.2 })?;
                    g.SetTrimEnd(trim)?;
                    let s = c.CreateSpriteShapeWithGeometry(&g)?;
                    s.SetStrokeBrush(&c.CreateColorBrushWithColor(col(*color))?)?;
                    s.SetStrokeThickness(*stroke)?;
                    s.SetStrokeStartCap(CompositionStrokeCap::Round)?;
                    s.SetStrokeEndCap(CompositionStrokeCap::Round)?;
                    // From the top, clockwise (the circle starts at its right).
                    s.SetCenterPoint(Vector2 { X: side / 2.0, Y: side / 2.0 })?;
                    s.SetRotationAngleInDegrees(-90.0)?;
                    v.Shapes()?.Append(&s)?;
                }
                kids.InsertAtTop(&v)?;
            }
        }
    }
    Ok(())
}

impl Card {
    unsafe fn new(req: Request) -> Result<Self> {
        let s = req.scale;
        let (_, _, h) = layout(&req.data.live, req.data.live.iter().any(|a| matches!(a, Activity::Media(_))));
        let pill = req.pill;
        // The pill's own monitor (its middle: a bar at the bottom ends on
        // the next display's edge).
        let at = POINT { x: (pill.rect.left + pill.rect.right) / 2, y: (pill.rect.top + pill.rect.bottom) / 2 };
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(MonitorFromPoint(at, MONITOR_DEFAULTTONEAREST), &mut mi);
        let work = mi.rcWork;
        let up = crate::topbar::at_bottom();
        let (rect, fit) = place(pill.rect, work, s, h, up);
        let m_px = margin_px(pill.rect, rect, s);
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            w!("NopBarIsland"),
            &HSTRING::from(t("settings.topbar.island")),
            WS_POPUP,
            rect.left - m_px,
            rect.top - m_px,
            rect.right - rect.left + m_px * 2,
            rect.bottom - rect.top + m_px * 2,
            None,
            None,
            Some(GetModuleHandleW(None)?.into()),
            None,
        )?;
        let built = (|| -> Result<Self> {
            let enable = BOOL(1);
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_HOSTBACKDROPBRUSH, &enable as *const _ as *const _, 4);
            let c = req.gfx.compositor.clone();
            let interop: ICompositorDesktopInterop = c.cast()?;
            let target = interop.CreateDesktopWindowTarget(hwnd, true)?;
            let ds = s * fit;
            let outer = c.CreateContainerVisual()?;
            outer.SetScale(Vector3 { X: ds, Y: ds, Z: 1.0 })?;
            outer.SetOffset(Vector3 { X: m_px as f32, Y: m_px as f32, Z: 0.0 })?;
            target.SetRoot(&outer)?;
            let anim = c.CreateContainerVisual()?;
            outer.Children()?.InsertAtTop(&anim)?;
            let shape = morph::Shape { x: 0.0, y: 0.0, w: W, h, r: req.theme.radii().panel.min(h / 2.0) };
            let plate = morph::Plate::new(&req.gfx, &req.theme, ds, pill_shape(pill.rect, pill.scale, rect, ds), shape, up)?;
            anim.Children()?.InsertAtTop(&plate.root)?;
            fill_ghost(&c, &plate.ghost, &pill.parts, 1.0 / fit.max(0.01))?;
            let content = [c.CreateSpriteVisual()?, c.CreateSpriteVisual()?];
            for (k, v) in content.iter().enumerate() {
                v.SetOpacity(if k == 0 { 1.0 } else { 0.0 })?;
                plate.content.Children()?.InsertAtTop(v)?;
            }
            let washes = [Wash::new(&c, &plate.content)?, Wash::new(&c, &plate.content)?];
            Ok(Card {
                hwnd,
                bar: req.bar,
                gfx: req.gfx.clone(),
                theme: req.theme.clone(),
                scale: s,
                fit,
                _target: target,
                outer,
                anim,
                plate,
                content,
                cur: 0,
                rows: Vec::new(),
                drawn: false,
                washes,
                wash: 0,
                icons: [Vec::new(), Vec::new()],
                rect,
                pill: pill.rect,
                shell: pill.shell,
                m_px,
                work,
                up,
                height: h,
                data: req.data,
                pinned: false,
                closing: false,
                settle: None,
                hide: None,
                shrink: None,
                hits: Vec::new(),
                shapes: Vec::new(),
                hover: None,
                pressed: None,
                focus: None,
                keyboard: false,
                tracking: false,
                rtl: crate::i18n::rtl(),
                press_at: None,
                dragging: false,
                sliding: None,
                drag_pos: None,
                seek_hold: None,
                seen_refused: None,
                vol_floor: 0,
                volume: None,
                volume_app: String::new(),
                vol_answered: false,
                sliders: Vec::new(),
                ticking: false,
                wheel_acc: 0,
            })
        })();
        if built.is_err() {
            let _ = DestroyWindow(hwnd);
        }
        built
    }

    fn ds(&self) -> f32 {
        self.scale * self.fit
    }

    /// The card as the plate's far end: its own corner at 0, 0.
    fn shape(&self) -> morph::Shape {
        morph::Shape { x: 0.0, y: 0.0, w: W, h: self.height, r: self.theme.radii().panel.min(self.height / 2.0) }
    }

    /// Out of the pill (7.2 rule 4): the plate, standing exactly on the
    /// pill, goes to the card on GLIDE; the bar's own pill is hidden once
    /// the plate is on screen over it. With reduced motion nothing changes
    /// shape (12.2): the card is there, fading in over REDUCED_MS as the
    /// pill fades out, in the same frame.
    fn open_motion(&mut self) {
        let c = self.gfx.compositor.clone();
        if !crate::motion::full() {
            self.plate.jump(1.0);
            let _ = crate::motion::fade_from(&c, &self.anim, crate::motion::Start::At(0.0), 1.0, crate::motion::REDUCED_MS, crate::motion::EASE_OUT);
            let _ = crate::motion::fade_to(&c, &self.shell, 0.0, crate::motion::REDUCED_MS, crate::motion::EASE_IN);
            return;
        }
        self.hide = crate::motion::after(
            HANDOVER_MS,
            Box::new(|| {
                with_card(|c| {
                    c.hide = None;
                    let _ = crate::motion::fade_to(&c.gfx.compositor, &c.shell, 0.0, 0, crate::motion::EASE_IN);
                });
            }),
        );
        self.go(1.0);
    }

    /// Sends the plate to `to` and sets the one-shot for its arrival.
    fn go(&mut self, to: f32) {
        if let Some(id) = self.settle.take() {
            crate::motion::cancel(id);
        }
        let ms = self.plate.go(to, crate::motion::SPRING_GLIDE);
        self.settle = crate::motion::after(ms.max(1), Box::new(arrived));
    }

    /// The settle one-shot fired. True: the card went back into the pill
    /// (it is over). Else it stands: exactly on its place.
    fn arrived(&mut self) -> bool {
        self.settle = None;
        if !self.plate.settled() {
            // Turned round meanwhile: look again when that arrives.
            let (_, to) = self.plate.progress();
            self.go(to);
            return false;
        }
        let (_, to) = self.plate.progress();
        self.plate.jump(to);
        if !self.closing && to > 0.5 {
            // It stands: what the pointer is on now (it may not have moved
            // since the content began to take it).
            self.rehover();
        }
        self.closing && to < 0.5
    }

    /// Back into the pill: no more input (clicks go through to whatever
    /// is under it, the bar's pill too: pressing that again turns it
    /// round), the plate's near end is the pill as it is now, and `p`
    /// goes to 0 from where it is. False if it could not be made
    /// click-through (it is closed at once then).
    fn fold(&mut self, pill: Pill) -> bool {
        if !unsafe { crate::motion::click_through(self.hwnd) } {
            return false;
        }
        self.closing = true;
        self.pinned = false;
        self.hover = None;
        self.pressed = None;
        self.sync_wash();
        unsafe {
            if GetCapture() == self.hwnd {
                let _ = ReleaseCapture();
            }
            a11y::menu_event(self.hwnd, false);
            if self.ticking {
                let _ = KillTimer(Some(self.hwnd), TICK_ID);
                self.ticking = false;
            }
        }
        self.take_pill(pill, true);
        self.go(0.0);
        true
    }

    /// Opened again while it was going back: it takes input again and the
    /// plate turns round where it is, at the speed it has.
    fn turn_round(&mut self, req: Request) {
        unsafe { crate::motion::click_solid(self.hwnd) };
        self.closing = false;
        self.tracking = false;
        self.theme = req.theme;
        self.data = req.data;
        self.take_pill(req.pill, false);
        self.ask_volume();
        self.settle_hold();
        self.relayout();
        self.redraw();
        self.sync_tick();
        self.go(1.0);
    }

    /// The pill as it is now: the plate's near end (unseen while the card
    /// stands). `ghost`: its content too (the plate is going back to it).
    fn take_pill(&mut self, pill: Pill, ghost: bool) {
        self.pill = pill.rect;
        self.shell = pill.shell;
        let shape = pill_shape(pill.rect, pill.scale, self.rect, self.ds());
        self.plate.set_pill(shape);
        if ghost {
            let _ = fill_ghost(&self.gfx.compositor, &self.plate.ghost, &pill.parts, 1.0 / self.fit.max(0.01));
        }
    }

    /// The bar's own pill shows again (over `ms`: 0 at once).
    fn show_pill(&self, ms: i64) {
        let _ = crate::motion::fade_to(&self.gfx.compositor, &self.shell, 1.0, ms, crate::motion::EASE_OUT);
    }

    fn cancel_timers(&mut self) {
        for id in [self.settle.take(), self.hide.take(), self.shrink.take()].into_iter().flatten() {
            crate::motion::cancel(id);
        }
    }

    /// The window around the card as it is placed now.
    fn fit_window(&mut self) {
        self.m_px = margin_px(self.pill, self.rect, self.scale);
        let (r, m) = (self.rect, self.m_px);
        unsafe {
            let _ = SetWindowPos(self.hwnd, None, r.left - m, r.top - m, r.right - r.left + m * 2, r.bottom - r.top + m * 2, SWP_NOZORDER | SWP_NOACTIVATE);
        }
        let _ = self.outer.SetOffset(Vector3 { X: m as f32, Y: m as f32, Z: 0.0 });
    }

    /// The rows' kinds and heights: another set crosses in (7.4).
    fn row_set(&self) -> Vec<(Row, u32)> {
        layout(&self.data.live, self.vol_row()).0.into_iter().map(|(r, _, h)| (r, h.to_bits())).collect()
    }

    /// The rows changed: the card is another height, or somewhere else
    /// (the pill moved). Under a bar at the top a new height glides (7.4:
    /// a panel's height on GLIDE, its edge at the bar fixed; the window
    /// grows before the plate does and shrinks after it); everything else
    /// is placed at once.
    fn relayout(&mut self) {
        let (_, _, h) = layout(&self.data.live, self.vol_row());
        let (rect, fit) = place(self.pill, self.work, self.scale, h, self.up);
        if h == self.height && rect == self.rect {
            return;
        }
        let glide = crate::motion::full() && self.drawn && !self.up && fit == self.fit && rect.left == self.rect.left && rect.top == self.rect.top;
        let grew = h > self.height;
        self.height = h;
        self.rect = rect;
        self.fit = fit;
        if let Some(id) = self.shrink.take() {
            crate::motion::cancel(id);
        }
        if glide {
            if grew {
                self.fit_window();
            }
            let ms = self.plate.glide_height(h);
            if !grew {
                self.shrink = crate::motion::after(
                    ms + 32,
                    Box::new(|| {
                        with_card(|c| {
                            c.shrink = None;
                            c.fit_window();
                        });
                    }),
                );
            }
            return;
        }
        self.fit_window();
        let ds = self.ds();
        let _ = self.outer.SetScale(Vector3 { X: ds, Y: ds, Z: 1.0 });
        self.plate.set_card(self.shape());
        let shape = pill_shape(self.pill, 1.0, self.rect, ds);
        self.plate.set_pill(shape);
    }

    // ---- what is playing: position and volume

    /// The keyboard's order now.
    fn order(&self) -> Vec<Target> {
        targets(&self.data.live, self.vol_level().is_some(), self.data.pip)
    }

    /// Hover, press and focus that are no longer there go; a held slider
    /// that went (or a seek dragged on another track) is let go of.
    /// Returns what is left to do outside the card's borrow.
    fn forget_gone(&mut self) -> Released {
        let order = self.order();
        for t in [&mut self.hover, &mut self.pressed, &mut self.focus] {
            // (The screenshot's picture is a drag handle, not in the order.)
            if t.is_some_and(|x| !order.contains(&x) && x != Target::Shot(0)) {
                *t = None;
            }
        }
        // A seek held or dragged for another track (or session) is over.
        let gen = self.gen();
        if self.seek_hold.is_some_and(|h| Some(h.0) != gen) {
            self.seek_hold = None;
        }
        let other_track = self.drag_pos.is_some_and(|d| Some(d.0) != gen);
        if self.sliding.is_some_and(|s| !order.contains(&s)) || other_track {
            return self.let_go(false);
        }
        Released::default()
    }

    fn track(&self) -> Option<&model::Track> {
        self.data.live.iter().find_map(|a| if let Activity::Media(tr) = a { Some(tr) } else { None })
    }

    /// The session and track shown (model::Track::gen).
    fn gen(&self) -> Option<u32> {
        self.track().map(|t| t.gen)
    }

    /// The track's timeline (one worth a bar) and whether it plays.
    fn timeline(&self) -> Option<(model::Timeline, bool)> {
        let tr = self.track()?;
        let tl = tr.timeline.filter(|t| t.shown())?;
        Some((tl, tr.playing))
    }

    /// The seek held now: one for this track, not past SEEK_HOLD.
    fn live_hold(&self) -> Option<(i64, Instant)> {
        let (gen, p, at) = self.seek_hold?;
        (Some(gen) == self.gen() && at.elapsed() < SEEK_HOLD).then_some((p, at))
    }

    /// Where the bar shows the track now: where it is dragged to, a seek
    /// just sent (moving on from there), or the app's report moved on.
    fn shown_pos(&self) -> Option<i64> {
        let (tl, playing) = self.timeline()?;
        if let Some((_, p)) = self.drag_pos.filter(|d| Some(d.0) == self.gen()) {
            return Some(p.clamp(tl.start_ms, tl.end_ms));
        }
        let now = Instant::now();
        if let Some((p, at)) = self.live_hold() {
            let elapsed = now.saturating_duration_since(at).as_millis().min(i64::MAX as u128) as i64;
            return Some(model::interpolate(p, elapsed, tl.rate, playing, tl.start_ms, tl.end_ms));
        }
        Some(tl.position(now, playing))
    }

    /// A seek's hold ends when the app's report agrees, the app refused
    /// it, the track changed, or after SEEK_HOLD.
    fn settle_hold(&mut self) {
        let refused = self.track().map(|t| t.seek_refused);
        if self.seen_refused.is_some() && refused != self.seen_refused {
            self.seek_hold = None;
        }
        self.seen_refused = refused;
        let Some((held, _)) = self.live_hold() else {
            self.seek_hold = None;
            return;
        };
        let Some((tl, playing)) = self.timeline() else {
            self.seek_hold = None;
            return;
        };
        let held_now = self.shown_pos().filter(|_| self.drag_pos.is_none()).unwrap_or(held);
        if (tl.position(Instant::now(), playing) - held_now).abs() < model::TIMELINE_SLACK_MS {
            self.seek_hold = None;
        }
    }

    /// Asks the app to play from `ms`, kept where it takes a seek (the main
    /// bar holds the player; the worker drops it if the track changed).
    fn seek(&mut self, ms: i64) {
        let (Some((tl, _)), Some(gen)) = (self.timeline(), self.gen()) else { return };
        let Some(ms) = tl.clamp_seek(ms) else { return };
        self.seek_hold = Some((gen, ms, Instant::now()));
        unsafe {
            let _ = PostMessageW(Some(self.bar), WM_ISLAND_ACT, WPARAM(ACT_SEEK), LPARAM(model::pack_seek(gen, ms)));
        }
    }

    /// The playing app's volume, if it is the one shown: (percent, muted).
    fn vol_level(&self) -> Option<(u8, bool)> {
        let app = &self.track()?.app;
        self.volume.as_ref().filter(|v| &v.0 == app).map(|v| (v.1, v.2))
    }

    /// Asks for the playing app's volume when it is another app than
    /// asked for last (the answer comes as WM_CARD_VOLUME).
    fn ask_volume(&mut self) {
        let app = self.track().map(|t| t.app.clone()).unwrap_or_default();
        if app == self.volume_app {
            return;
        }
        self.volume = None;
        self.vol_answered = false;
        self.volume_app = app;
        if !self.volume_app.is_empty() {
            self.vol_floor = session::app_volume(self.hwnd, WM_CARD_VOLUME, &self.volume_app, None);
        }
    }

    /// The audio worker answered (posted). Only answers to this card's
    /// requests since the newest change count (an older one would undo
    /// what the slider, a key or the wheel just set).
    fn take_volume(&mut self) -> Released {
        let mut newest: Option<session::AppVolume> = None;
        for r in session::take_app_volume() {
            if volume_answer_counts(&r, &self.volume_app, self.vol_floor) && newest.as_ref().is_none_or(|n| r.seq >= n.seq) {
                newest = Some(r);
            }
        }
        let Some(r) = newest else { return Released::default() };
        // (No audio session: its row goes.)
        self.vol_answered = true;
        self.volume = r.level.map(|(p, m)| (r.app, p, m));
        self.forget_gone()
    }

    /// What is playing has its volume row: its app's volume is known, or
    /// it has not answered yet (the row is kept meanwhile: the card does
    /// not grow a moment after it opens).
    fn vol_row(&self) -> bool {
        self.track().is_some() && (self.vol_level().is_some() || !self.vol_answered)
    }

    /// Sets the app's volume to `p` percent (shown at once, then asked).
    fn set_volume(&mut self, p: i32) {
        let Some((app, cur, muted)) = self.volume.clone() else { return };
        let p = p.clamp(0, 100) as u8;
        if p == cur {
            return;
        }
        // Turning it up unmutes it (the worker does the same).
        self.volume = Some((app.clone(), p, muted && p < cur));
        self.vol_floor = session::app_volume(self.hwnd, WM_CARD_VOLUME, &app, Some(p));
    }

    /// The held slider follows the pointer (`x`: card DIP, as drawn).
    fn slide_to(&mut self, tg: Target, x: f32) {
        let Some(&(_, x0, x1)) = self.sliders.iter().find(|s| s.0 == tg) else { return };
        match tg {
            Target::Seek => {
                if let (Some((tl, _)), Some(gen)) = (self.timeline(), self.gen()) {
                    // Shown where the pointer is, kept where a seek may go.
                    let at = model::seek_at(self.rtl, x, x0, x1, &tl);
                    self.drag_pos = tl.clamp_seek(at).map(|p| (gen, p));
                }
            }
            Target::Volume => {
                let v = crate::i18n::mirror::slider_value(self.rtl, x, x0, x1);
                self.set_volume((v * 100.0).round() as i32);
            }
            _ => {}
        }
    }

    /// Lets go of the held slider, however it ended: the button came up
    /// (`commit`: the position bar seeks where it was dragged, if still
    /// on the same track), its target went, the track changed, or the
    /// mouse was taken. What is left for outside the borrow: a hover
    /// card's capture to release, and its leave check when the pointer is
    /// outside (leaving was not reported while the slider was held).
    fn let_go(&mut self, commit: bool) -> Released {
        let Some(tg) = self.sliding.take() else { return Released::default() };
        let drag = self.drag_pos.take();
        if commit && tg == Target::Seek {
            if let Some((_, p)) = drag.filter(|d| Some(d.0) == self.gen()) {
                self.seek(p);
            }
        }
        let mut pt = POINT::default();
        let away = unsafe { GetCursorPos(&mut pt) }.is_ok() && self.spot_on_screen(pt) == Spot::Outside;
        Released { hwnd: Some(self.hwnd), capture: !self.pinned, leave: (!self.pinned && away).then_some(self.bar) }
    }

    /// Left / Right on a focused slider (in reading order: right to left,
    /// Left moves it on).
    fn nudge(&mut self, tg: Target, forward: bool) {
        match tg {
            Target::Seek => {
                if let (Some((tl, _)), Some(p)) = (self.timeline(), self.shown_pos()) {
                    self.seek(tl.step(p, if forward { model::SEEK_STEP_MS } else { -model::SEEK_STEP_MS }));
                }
            }
            Target::Volume => {
                if let Some((p, _)) = self.vol_level() {
                    self.set_volume(p as i32 + if forward { VOL_STEP } else { -VOL_STEP });
                }
            }
            _ => {}
        }
    }

    /// The card's tick: just after the time played next changes (while
    /// the track plays with a bar), and when a seek's hold runs out (even
    /// paused: the bar must not keep a seek the app never made). Off
    /// otherwise.
    fn sync_tick(&mut self) {
        let play = self.timeline().filter(|(_, playing)| *playing).and_then(|(tl, _)| Some(model::ms_to_next_second(self.shown_pos()? - tl.start_ms, tl.rate)));
        let hold = self.live_hold().map(|(_, at)| SEEK_HOLD.saturating_sub(at.elapsed()));
        unsafe {
            match next_tick(play, hold) {
                Some(ms) => {
                    SetTimer(Some(self.hwnd), TICK_ID, ms, None);
                    self.ticking = true;
                }
                None if self.ticking => {
                    let _ = KillTimer(Some(self.hwnd), TICK_ID);
                    self.ticking = false;
                }
                None => {}
            }
        }
    }

    // ---- colors

    fn hc(&self) -> Option<crate::syslook::HighContrast> {
        self.theme.hc.filter(|_| self.theme.high_contrast)
    }
    fn ink(&self) -> D2D1_COLOR_F {
        rgba(self.theme.ink())
    }
    /// Secondary text, translucent, raised only as far as 4.5:1 on the
    /// card and its rows needs over any wallpaper (control::readable_dim).
    fn dim(&self) -> D2D1_COLOR_F {
        let t = &self.theme;
        let fills = [t.fill_of(Kind::Row, State::Hover), t.fill_of(Kind::Row, State::Pressed), t.fill_track()];
        rgba(crate::topbar::control::readable_dim(t.text_secondary(), t.text_primary().a, &crate::topbar::control::panel_extremes(t), &fills))
    }
    fn accent(&self) -> D2D1_COLOR_F {
        match self.hc() {
            Some(h) => rgba(h.highlight),
            None => rgba(self.theme.accent()),
        }
    }
    fn tone(&self, t: model::Tone) -> D2D1_COLOR_F {
        rgba(super::tone_rgba(t, &self.theme))
    }

    // ---- drawing helpers

    fn layout_text(&self, text: &str, size: f32, bold: bool) -> Option<(IDWriteTextLayout, f32, f32)> {
        let weight = if bold { DWRITE_FONT_WEIGHT_SEMI_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL };
        self.gfx.text_layout(text, &self.theme.font.family, size, weight).ok()
    }

    /// Text cut with "…" to fit `maxw` DIP.
    fn fit_text(&self, text: &str, size: f32, bold: bool, maxw: f32) -> Option<(IDWriteTextLayout, f32, f32)> {
        let l = self.layout_text(text, size, bold)?;
        if l.1 <= maxw {
            return Some(l);
        }
        let chars: Vec<char> = text.chars().collect();
        let (mut lo, mut hi) = (0usize, chars.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if self.layout_text(&model::truncate(text, mid + 1), size, bold).is_some_and(|l| l.1 <= maxw) {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        self.layout_text(&model::truncate(text, lo + 1), size, bold)
    }

    /// Like num_text, semibold (the timer's time).
    fn num_strong(&self, text: &str, size: f32) -> Option<(IDWriteTextLayout, f32, f32)> {
        self.num_weighted(text, size, true)
    }

    /// Numbers that keep their width as they change (tabular figures).
    fn num_text(&self, text: &str, size: f32) -> Option<(IDWriteTextLayout, f32, f32)> {
        self.num_weighted(text, size, false)
    }

    fn num_weighted(&self, text: &str, size: f32, bold: bool) -> Option<(IDWriteTextLayout, f32, f32)> {
        let (l, w, h) = self.layout_text(text, size, bold)?;
        unsafe {
            let Ok(typo) = self.gfx.dwrite().CreateTypography() else { return Some((l, w, h)) };
            let feature = DWRITE_FONT_FEATURE { nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES, parameter: 1 };
            let range = DWRITE_TEXT_RANGE { startPosition: 0, length: text.encode_utf16().count() as u32 };
            if typo.AddFontFeature(feature).is_ok() && l.SetTypography(&typo, range).is_ok() {
                let mut m = DWRITE_TEXT_METRICS::default();
                if l.GetMetrics(&mut m).is_ok() {
                    return Some((l, m.widthIncludingTrailingWhitespace.ceil(), h));
                }
            }
        }
        Some((l, w, h))
    }

    /// x of something `w` wide in the box `l..r`, at its reading start.
    fn start_x(&self, l: f32, r: f32, w: f32) -> f32 {
        if self.rtl { r - w } else { l }
    }

    /// x of something `w` wide in the box `l..r`, at its reading end.
    fn end_x(&self, l: f32, r: f32, w: f32) -> f32 {
        if self.rtl { l } else { r - w }
    }

    /// A box `l..r` (LTR) mirrored in a right-to-left language.
    fn mx(&self, l: f32, r: f32) -> (f32, f32) {
        crate::i18n::mirror::edges(self.rtl, l, r, 0.0, W)
    }

    /// Text centered on `cy`.
    unsafe fn text(&self, dc: &ID2D1DeviceContext, l: &(IDWriteTextLayout, f32, f32), x: f32, cy: f32, c: D2D1_COLOR_F) -> Result<()> {
        let b = dc.CreateSolidColorBrush(&c, None)?;
        crate::gfx::draw_text(dc, Vector2 { X: x, Y: cy - l.2 / 2.0 }, &l.0, &b, D2D1_DRAW_TEXT_OPTIONS_NONE);
        Ok(())
    }

    /// Text in the box `l..r` (LTR coordinates; mirrored and aligned to
    /// the reading start).
    #[allow(clippy::too_many_arguments)]
    unsafe fn text_in(&self, dc: &ID2D1DeviceContext, s: &str, size: f32, bold: bool, l: f32, r: f32, cy: f32, c: D2D1_COLOR_F) -> Result<f32> {
        let (bl, br) = self.mx(l, r);
        let Some(lay) = self.fit_text(s, size, bold, (br - bl).max(1.0)) else { return Ok(0.0) };
        self.text(dc, &lay, self.start_x(bl, br, lay.1), cy, c)?;
        Ok(lay.1)
    }

    /// Text ending at `r` (LTR coordinates; the reading end, mirrored).
    /// Returns its width.
    unsafe fn text_end(&self, dc: &ID2D1DeviceContext, l: Option<(IDWriteTextLayout, f32, f32)>, r: f32, cy: f32, c: D2D1_COLOR_F) -> Result<f32> {
        let Some(l) = l else { return Ok(0.0) };
        let (bl, br) = self.mx(r - l.1, r);
        self.text(dc, &l, self.end_x(bl, br, l.1), cy, c)?;
        Ok(l.1)
    }

    unsafe fn glyph(&self, dc: &ID2D1DeviceContext, g: Glyph, size: f32, cx: f32, cy: f32, c: D2D1_COLOR_F) -> Result<()> {
        crate::gfx::draw_glyph(dc, g, cx, cy, size, c)
    }

    /// A glyph in a two-line row's icon box (LTR x of the box's start).
    unsafe fn lead(&self, dc: &ID2D1DeviceContext, g: Glyph, y: f32, c: D2D1_COLOR_F) -> Result<()> {
        let (l, r) = self.mx(LINE, LINE + ICON);
        self.glyph(dc, g, GLYPH_L, (l + r) / 2.0, y + ROW / 2.0, c)
    }

    unsafe fn fill_rr(&self, dc: &ID2D1DeviceContext, c: D2D1_COLOR_F, r: D2D_RECT_F, rad: f32) -> Result<()> {
        if c.a > 0.0 {
            let b = dc.CreateSolidColorBrush(&c, None)?;
            dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: rad, radiusY: rad }, &b);
        }
        Ok(())
    }

    unsafe fn stroke_rr(&self, dc: &ID2D1DeviceContext, c: D2D1_COLOR_F, r: D2D_RECT_F, rad: f32, width: f32) -> Result<()> {
        let b = dc.CreateSolidColorBrush(&c, None)?;
        let h = width / 2.0;
        dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect(r.left + h, r.top + h, r.right - h, r.bottom - h), radiusX: (rad - h).max(0.0), radiusY: (rad - h).max(0.0) }, &b, width, None);
        Ok(())
    }

    /// Key `vk`: Left / Right move a focused slider, Tab and the arrows
    /// move the focus, Esc dismisses, Enter and Space act (what to run,
    /// after the card's borrow). The ring shows once the keyboard moves
    /// the focus (crate::input::navigates), never on another key.
    fn on_key(&mut self, vk: u16) -> Option<(HWND, usize, isize)> {
        let order = self.order();
        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
        if crate::input::navigates(vk) {
            self.keyboard = true;
        }
        // Left / Right move a focused slider (not the focus).
        if let Some(tg) = self.focus.filter(|f| f.slider() && order.contains(f)) {
            if vk == VK_LEFT.0 || vk == VK_RIGHT.0 {
                self.nudge(tg, crate::i18n::mirror::arrow(self.rtl, vk == VK_RIGHT.0) > 0);
                self.redraw();
                self.publish();
                self.sync_tick();
                return None;
            }
        }
        let dir = match vk {
            v if v == VK_TAB.0 => Some(if shift { -1 } else { 1 }),
            v if v == VK_DOWN.0 => Some(1),
            v if v == VK_UP.0 => Some(-1),
            v if v == VK_RIGHT.0 => Some(crate::i18n::mirror::arrow(self.rtl, true)),
            v if v == VK_LEFT.0 => Some(crate::i18n::mirror::arrow(self.rtl, false)),
            _ => None,
        };
        if let Some(d) = dir {
            self.focus = step_focus(&order, self.focus, d);
            self.redraw();
            self.publish();
            return None;
        }
        if vk == VK_ESCAPE.0 {
            return Some((self.bar, ACT_DISMISS, 0));
        }
        if vk == VK_RETURN.0 || vk == VK_SPACE.0 {
            if self.focus.is_some() {
                self.redraw();
            }
            return self.focus.and_then(|f| self.action(f));
        }
        None
    }

    /// What the pointer is doing to `tg` (6.2).
    fn state_of(&self, tg: Target) -> State {
        if self.pressed == Some(tg) {
            State::Pressed
        } else if self.hover == Some(tg) {
            State::Hover
        } else {
            State::Rest
        }
    }

    /// Something that can be pressed, `r` (corners `rad`): its box for the
    /// pointer and for the wash (the hover and pressed fill is a visual
    /// under the picture: sync_wash), and what the picture itself draws
    /// of its state: high contrast's Highlight (a 2 DIP outline hovered, a
    /// fill pressed: 12.3) and the keyboard's focus ring (6.4: 2 wide, 2
    /// out, only after a key). Returns its state: what its glyph is drawn
    /// in.
    unsafe fn target(&self, dc: &ID2D1DeviceContext, out: &mut Painted, tg: Target, r: D2D_RECT_F, rad: f32) -> Result<State> {
        out.hits.push((r, tg));
        out.shapes.push((tg, r, rad));
        let st = self.state_of(tg);
        if self.hc().is_some() {
            self.fill_rr(dc, rgba(self.theme.fill_of(Kind::Row, st)), r, rad)?;
            if let Some((c, w)) = self.theme.outline_of(Kind::Row, st) {
                self.stroke_rr(dc, rgba(c), r, rad, w)?;
            }
        }
        if self.keyboard && self.focus == Some(tg) {
            crate::gfx::parts::focus_ring(dc, r, rad, &self.theme, 1.0)?;
        }
        Ok(st)
    }

    /// A round glyph button at LTR `x` (its left edge), centered on `cy`:
    /// a 28 capsule, a 16 glyph. Not `on`: drawn in the disabled ink, not
    /// a target.
    #[allow(clippy::too_many_arguments)]
    unsafe fn button(&self, dc: &ID2D1DeviceContext, out: &mut Painted, tg: Target, g: Glyph, x: f32, cy: f32, on: bool) -> Result<()> {
        let (l, r) = self.mx(x, x + BTN);
        let st = if on { self.target(dc, out, tg, rect(l, cy - BTN / 2.0, r, cy + BTN / 2.0), crate::theme::capsule(BTN))? } else { State::Disabled };
        self.glyph(dc, g, GLYPH, (l + r) / 2.0, cy, rgba(self.theme.text_of(Level::Primary, st)))
    }

    /// A bar that only shows a share (a job's progress, the battery, the
    /// timer): a TRACK_H well with round ends from LTR `l` to `r`, its
    /// share `v` filled from the reading start in `fill`.
    unsafe fn bar(&self, dc: &ID2D1DeviceContext, l: f32, r: f32, top: f32, v: f32, fill: D2D1_COLOR_F) -> Result<()> {
        let (bl, br) = self.mx(l, r);
        let track = rect(bl, top, br, top + TRACK_H);
        crate::gfx::parts::well(dc, track, crate::theme::capsule(TRACK_H), &self.theme, 1.0)?;
        let w = (br - bl) * v.clamp(0.0, 1.0);
        let (fl, fr) = if self.rtl { (br - w, br) } else { (bl, bl + w) };
        if w > 0.5 {
            self.fill_rr(dc, fill, rect(fl, track.top, fr, track.bottom), crate::theme::capsule(TRACK_H))?;
        }
        Ok(())
    }

    /// A slider: a TRACK_H well with round ends from `x0` to `x1` (as
    /// drawn) centered on `cy`, its share `v` filled from the reading
    /// start in `fill`, and its knob (16, E1 and the contact shadow; held,
    /// KNOB_HELD larger: 6.2). `live`: it can be moved. `always`: the knob
    /// shows at rest too (the volume); else only while hovered, held or
    /// moved by a key (the position bar reads as a progress bar until
    /// then). The keyboard's ring goes round the knob. Returns its grip
    /// (the pointer's target).
    #[allow(clippy::too_many_arguments)]
    unsafe fn slim(&self, dc: &ID2D1DeviceContext, tg: Target, x0: f32, x1: f32, cy: f32, v: f32, fill: D2D1_COLOR_F, live: bool, always: bool) -> Result<D2D_RECT_F> {
        let half = TRACK_H / 2.0;
        let track = rect(x0, cy - half, x1, cy + half);
        let grip = rect(x0 - KNOB_R, cy - GRIP_H / 2.0, x1 + KNOB_R, cy + GRIP_H / 2.0);
        let keyed = live && self.keyboard && self.focus == Some(tg);
        crate::gfx::parts::well(dc, track, half, &self.theme, 1.0)?;
        let kx = crate::i18n::mirror::slider_x(self.rtl, v, x0, x1);
        let (fl, fr) = if self.rtl { (kx, x1) } else { (x0, kx) };
        if fr - fl > 0.5 {
            self.fill_rr(dc, fill, rect(fl, track.top, fr, track.bottom), half)?;
        }
        let held = self.sliding == Some(tg);
        if live && (always || self.hover == Some(tg) || held || keyed) {
            let r = KNOB_R * if held { size::KNOB_HELD } else { 1.0 };
            crate::gfx::parts::knob(dc, kx, cy, r, &self.theme, false, 1.0)?;
            if keyed {
                // (Glass's knob is a capsule: the ring goes round its width.)
                let hw = crate::gfx::parts::knob_half_w(r, &self.theme);
                crate::gfx::parts::focus_ring(dc, rect(kx - hw, cy - r, kx + hw, cy + r), r, &self.theme, 1.0)?;
            }
        }
        Ok(grip)
    }

    /// The played part's color: the theme's "on" fill (the system
    /// highlight in high contrast); `quiet` (read only, muted) is dimmer.
    fn slim_fill(&self, quiet: bool) -> D2D1_COLOR_F {
        match (self.hc(), quiet) {
            (Some(h), false) => rgba(h.highlight),
            (Some(_), true) => self.ink(),
            (None, false) => rgba(self.theme.fill()),
            (None, true) => self.dim(),
        }
    }

    /// Album art in a rounded square (never mirrored), or a note on a
    /// card fill. A picture is content, not a vessel: its corners go by
    /// its size (ART_R), not by where it sits (3.3).
    unsafe fn art(&self, dc: &ID2D1DeviceContext, r: D2D_RECT_F) -> Result<()> {
        let rad = size::ART_R;
        if let Some(a) = &self.data.art {
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            if a.pixels.len() == (a.px * a.px * 4) as usize {
                let bmp = dc.CreateBitmap(D2D_SIZE_U { width: a.px, height: a.px }, Some(a.pixels.as_ptr() as _), a.px * 4, &props)?;
                let k = (r.right - r.left) / a.px as f32;
                let bprops = D2D1_BITMAP_BRUSH_PROPERTIES1 { extendModeX: D2D1_EXTEND_MODE_CLAMP, extendModeY: D2D1_EXTEND_MODE_CLAMP, interpolationMode: D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC };
                let brush_props = D2D1_BRUSH_PROPERTIES { opacity: 1.0, transform: Matrix3x2 { M11: k, M12: 0.0, M21: 0.0, M22: k, M31: r.left, M32: r.top } };
                let b = dc.CreateBitmapBrush(&bmp, Some(&bprops), Some(&brush_props))?;
                dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: rad, radiusY: rad }, &b);
                if self.hc().is_some() {
                    self.stroke_rr(dc, self.ink(), r, rad, 1.0)?;
                }
                return Ok(());
            }
        }
        match self.theme.outline_of(Kind::Raised, State::Rest) {
            Some((c, w)) => self.stroke_rr(dc, rgba(c), r, rad, w)?,
            // (Raised: Flat v2's card light round it.)
            None => crate::gfx::parts::card(dc, r, rad, &self.theme, State::Rest, self.theme.fill_of(Kind::Raised, State::Rest), 1.0, 1.0)?,
        }
        self.glyph(dc, G_MUSIC, GLYPH_L, (r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0, rgba(self.theme.text_of(Level::Tertiary, State::Rest)))
    }

    // ---- the content

    fn redraw(&mut self) {
        let s = self.ds();
        let (pw, ph) = ((W * s).ceil() as u32, (self.height * s).ceil() as u32);
        let mut out = Painted::default();
        let me = &*self;
        let surface = self.gfx.draw(pw, ph, |dc, ox, oy| unsafe {
            dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            dc.SetTransform(&Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: ox, M32: oy });
            let r = me.paint(dc, &mut out);
            dc.SetTransform(&Matrix3x2::identity());
            r
        });
        self.hits = out.hits;
        self.shapes = out.shapes;
        self.sliders = out.sliders;
        // Another set of rows crosses in (7.4: the old picture out over
        // FADE_OUT_MS, the new one in over CROSS_MS, from the same frame,
        // nothing moving); the same rows are drawn in place.
        let rows = self.row_set();
        let cross = self.drawn && rows != self.rows;
        self.rows = rows;
        self.drawn = true;
        let c = self.gfx.compositor.clone();
        if cross {
            let _ = crate::motion::fade_to(&c, &self.content[self.cur], 0.0, crate::motion::len(crate::motion::FADE_OUT_MS), crate::motion::EASE_IN);
            self.cur = 1 - self.cur;
            let _ = crate::motion::fade_from(&c, &self.content[self.cur], crate::motion::Start::At(0.0), 1.0, crate::motion::len(crate::motion::CROSS_MS), crate::motion::EASE_OUT);
        }
        match surface {
            Ok(surface) => {
                if let Ok(brush) = c.CreateSurfaceBrushWithSurface(&surface) {
                    let _ = brush.SetStretch(CompositionStretch::Fill);
                    let v = &self.content[self.cur];
                    let _ = v.SetBrush(&brush);
                    let _ = v.SetSize(Vector2 { X: W, Y: self.height });
                }
            }
            Err(e) => crate::log!("island: card draw failed: {e}"),
        }
        self.place_icons(out.icons);
        self.sync_wash();
    }

    /// The dock's icons over the progress rows: children of the picture
    /// that shows (they cross with it: the picture fading out keeps its
    /// own until it has; only this one's are made again).
    fn place_icons(&mut self, want: Vec<(String, D2D_RECT_F)>) {
        let cur = self.cur;
        for v in self.icons[cur].drain(..) {
            if let Ok(p) = v.Parent() {
                let _ = p.Children().and_then(|c| c.Remove(&v));
            }
        }
        let Ok(ch) = self.content[cur].Children() else { return };
        for (key, r) in want {
            let Some(brush) = self.data.icons.get(&key) else { continue };
            let Ok(v) = self.gfx.compositor.CreateSpriteVisual() else { continue };
            let _ = v.SetBrush(brush);
            let _ = v.SetSize(Vector2 { X: r.right - r.left, Y: r.bottom - r.top });
            let _ = v.SetOffset(Vector3 { X: r.left, Y: r.top, Z: 0.0 });
            if ch.InsertAtTop(&v).is_ok() {
                self.icons[cur].push(v);
            }
        }
    }

    /// The fill under what the pointer is on (6.2, 6.3): a row fill on
    /// its target's shape, coming on over FADE_IN_MS and going over
    /// FADE_OUT_MS (the one it left fades out while the next fades in);
    /// a press is there at once, and its size goes to press_scale on
    /// SNAP. Sliders have none (a sunk thing answers a touch, not the
    /// pointer); high contrast draws its own in the picture, at once.
    fn sync_wash(&mut self) {
        let want = self
            .pressed
            .map(|t| (t, true))
            .or(self.hover.map(|t| (t, false)))
            .filter(|(t, _)| !t.slider() && *t != Target::Shot(0) && self.hc().is_none())
            .and_then(|(t, down)| self.shapes.iter().find(|s| s.0 == t).map(|s| WashOn { t, down, r: s.1, rad: s.2 }));
        let c = self.gfx.compositor.clone();
        let now = self.washes[self.wash].on;
        let (over, held) = (self.theme.fill_of(Kind::Row, State::Hover), self.theme.fill_of(Kind::Row, State::Pressed));
        let fill = move |down: bool| if down { held } else { over };
        match wash_step(now, want) {
            WashStep::Stay => {}
            WashStep::Other => {
                if now.is_some() {
                    let w = &mut self.washes[self.wash];
                    let _ = crate::motion::fade_to(&c, &w.v, 0.0, crate::motion::FADE_OUT_MS, crate::motion::EASE_IN);
                    w.on = None;
                    self.wash = 1 - self.wash;
                }
                let Some(on) = want else { return };
                let w = &mut self.washes[self.wash];
                let _ = w.v.StopAnimation(&HSTRING::from("Scale.X"));
                let _ = w.v.StopAnimation(&HSTRING::from("Scale.Y"));
                let _ = w.v.SetScale(Vector3 { X: 1.0, Y: 1.0, Z: 1.0 });
                w.place(on.r, on.rad);
                super::color_to(&c, &w.brush, fill(on.down), 0);
                let _ = crate::motion::fade_to(&c, &w.v, 1.0, if on.down { 0 } else { crate::motion::FADE_IN_MS }, crate::motion::EASE_OUT);
                if on.down {
                    let _ = crate::motion::press(&c, &w.v, (on.r.right - on.r.left, on.r.bottom - on.r.top), true);
                }
                w.on = Some(on);
            }
            // The same target: pressed or let go, or moved with its row.
            WashStep::Same { press, moved } => {
                let Some(on) = want else { return };
                let w = &mut self.washes[self.wash];
                if moved {
                    w.place(on.r, on.rad);
                }
                if press {
                    super::color_to(&c, &w.brush, fill(on.down), if on.down { 0 } else { crate::motion::FADE_IN_MS });
                    let _ = crate::motion::press(&c, &w.v, (on.r.right - on.r.left, on.r.bottom - on.r.top), on.down);
                }
                w.on = Some(on);
            }
        }
    }

    unsafe fn paint(&self, dc: &ID2D1DeviceContext, out: &mut Painted) -> Result<()> {
        let (rows, seps, _) = layout(&self.data.live, self.vol_row());
        let (ink, dim) = (self.ink(), self.dim());
        let end = W - LINE;
        for y in seps {
            // One device pixel on a whole pixel (not a DIP straddling two),
            // from content line to content line.
            let c = rgba(self.theme.hairline());
            let ds = self.ds().max(0.1);
            let top = (y * ds).round();
            self.fill_rr(dc, c, rect(LINE, top / ds, end, (top + 1.0) / ds), 0.0)?;
        }
        for (row, y, h) in rows {
            match row {
                Row::Job(ai, j) => {
                    let Some(Activity::Progress(jobs)) = self.data.live.get(ai) else { continue };
                    let Some(job) = jobs.get(j) else { continue };
                    if job.hwnd != 0 {
                        // The whole row, from vessel line to vessel line.
                        self.target(dc, out, Target::Job(ai, j), rect(PAD, y, W - PAD, y + h), self.theme.radii().card)?;
                    }
                    // Icon (the dock's, drawn above), else a glyph.
                    let (il, ir) = self.mx(LINE, LINE + ICON);
                    if self.data.icons.contains_key(&job.key) {
                        out.icons.push((job.key.clone(), rect(il, y + (h - ICON) / 2.0, ir, y + (h + ICON) / 2.0)));
                    } else {
                        self.lead(dc, if job.files { Glyph::Copy } else { Glyph::Download }, y, ink)?;
                    }
                    let (name_cy, bar_top) = bar_lines(y);
                    let pct = crate::i18n::tf("control.percent", &[("p", &model::percent(job.ratio).to_string())]);
                    let pw = self.text_end(dc, self.num_text(&pct, SMALL), end, name_cy, dim)?;
                    self.text_in(dc, &model::job_title(job), TEXT, true, TEXT_X, end - pw - GAP, name_cy, ink)?;
                    self.bar(dc, TEXT_X, end, bar_top, job.ratio, self.accent())?;
                }
                Row::Media(ai) => {
                    let Some(Activity::Media(tr)) = self.data.live.get(ai) else { continue };
                    let top = y + size::CARD_PAD;
                    let (al, ar) = self.mx(LINE, LINE + ART);
                    self.art(dc, rect(al, top, ar, top + ART))?;
                    // The buttons at the end of the art's line: the PiP,
                    // previous, play / pause, next. Previous and next swap
                    // glyphs right to left (they point the reading way).
                    let mut buttons: Vec<(Target, Glyph, bool)> = Vec::new();
                    if self.data.pip {
                        buttons.push((Target::Pip, crate::pip::G_PIP, true));
                    }
                    buttons.extend([(Target::Prev, G_PREV, tr.can_prev), (Target::Toggle, if tr.playing { G_PAUSE } else { G_PLAY }, tr.can_toggle), (Target::Next, G_NEXT, tr.can_next)]);
                    let xs = buttons_from_end(buttons.len());
                    for ((tg, g, on), x) in buttons.into_iter().zip(&xs) {
                        let g = match (self.rtl, g) {
                            (true, G_PREV) => G_NEXT,
                            (true, G_NEXT) => G_PREV,
                            (_, g) => g,
                        };
                        self.button(dc, out, tg, g, *x, top + ART / 2.0, on)?;
                    }
                    // Title and artist, centered beside the art.
                    let tx = LINE + ART + space::S4;
                    let words_r = xs.first().copied().unwrap_or(end) - GAP;
                    let title = if tr.title.trim().is_empty() { t("control.now_playing").to_string() } else { tr.title.clone() };
                    if tr.artist.trim().is_empty() {
                        self.text_in(dc, &title, TEXT, true, tx, words_r, top + ART / 2.0, ink)?;
                    } else {
                        let lines = ramp::BODY.line + ramp::FOOTNOTE.line;
                        let first = top + (ART - lines) / 2.0;
                        self.text_in(dc, &title, TEXT, true, tx, words_r, first + ramp::BODY.line / 2.0, ink)?;
                        self.text_in(dc, &tr.artist, SMALL, false, tx, words_r, first + ramp::BODY.line + ramp::FOOTNOTE.line / 2.0, dim)?;
                    }
                    let mut next = top + ART + GAP;
                    // The position bar: time played, the bar, the length.
                    if let (true, Some((tl, _)), Some(pos)) = (has_bar(tr), self.timeline(), self.shown_pos()) {
                        let cy = next + TIME_H / 2.0;
                        let (played, length) = model::time_pair(&tl, pos);
                        let lp = self.num_text(&played, TIME);
                        let ll = self.num_text(&length, TIME);
                        // Both ends as wide as the wider: the bar never moves.
                        let slot = lp.as_ref().map(|l| l.1).unwrap_or(0.0).max(ll.as_ref().map(|l| l.1).unwrap_or(0.0));
                        if let Some(l) = &lp {
                            let (sl, sr) = self.mx(LINE, LINE + slot);
                            self.text(dc, l, self.start_x(sl, sr, l.1), cy, dim)?;
                        }
                        self.text_end(dc, ll, end, cy, dim)?;
                        let (x0, x1) = self.mx(LINE + slot + GAP, end - slot - GAP);
                        let grip = self.slim(dc, Target::Seek, x0, x1, cy, tl.ratio(pos), self.slim_fill(!tl.can_seek()), tl.can_seek(), false)?;
                        if tl.can_seek() {
                            out.hits.push((grip, Target::Seek));
                            out.sliders.push((Target::Seek, x0, x1));
                        }
                        next += TIME_H + GAP;
                    }
                    // The app's own volume: a speaker at the content line
                    // and a slider to its mirror (the knob kept inside it).
                    if self.vol_row() {
                        let cy = next + VOL_H / 2.0;
                        let level = self.vol_level();
                        let (p, muted) = level.unwrap_or((0, false));
                        let (gl, gr) = self.mx(LINE, LINE + GLYPH);
                        self.glyph(dc, super::super::status::volume_glyph(p, muted), GLYPH, (gl + gr) / 2.0, cy, dim)?;
                        let (x0, x1) = self.mx(LINE + GLYPH + GAP + KNOB_R, end - KNOB_R);
                        let grip = self.slim(dc, Target::Volume, x0, x1, cy, p as f32 / 100.0, self.slim_fill(muted), level.is_some(), true)?;
                        if level.is_some() {
                            out.hits.push((grip, Target::Volume));
                            out.sliders.push((Target::Volume, x0, x1));
                        }
                    }
                }
                Row::Battery(ai) => {
                    let Some(Activity::Battery(p)) = self.data.live.get(ai) else { continue };
                    let (g, tone, pct) = match p {
                        Power::Charging(v) => (G_BOLT, model::Tone::Ink, *v),
                        Power::Low(v) => (G_BATTERY_LOW, model::Tone::Red, *v),
                    };
                    self.lead(dc, g, y, self.tone(tone))?;
                    let (name_cy, bar_top) = bar_lines(y);
                    let words = model::pill_words(&Activity::Battery(*p)).0;
                    self.text_in(dc, &words, TEXT, true, TEXT_X, end, name_cy, ink)?;
                    // Its charge as the row's bar.
                    self.bar(dc, TEXT_X, end, bar_top, pct.min(100) as f32 / 100.0, self.tone(tone))?;
                }
                Row::Timer(ai) => {
                    let Some(a @ Activity::Timer(tm)) = self.data.live.get(ai) else { continue };
                    let buttons = tm.focus.as_ref().is_some_and(|v| v.phase < 2);
                    self.lead(dc, G_TIMER, y, self.tone(model::shown_tone(a)))?;
                    let today = crate::i18n::tf("timer.today", &[("n", &tm.focus.as_ref().map(|v| v.today).unwrap_or(0).to_string())]);
                    if !buttons {
                        // Over, or a plain timer: its name, and under it
                        // today's count (the end notice).
                        if tm.focus.is_some() {
                            let (name_cy, note_cy) = note_lines(y);
                            self.text_in(dc, &model::timer_label(tm), TEXT, true, TEXT_X, end, name_cy, ink)?;
                            self.text_in(dc, &today, SMALL, false, TEXT_X, end, note_cy, dim)?;
                        } else {
                            let time = if tm.over() { String::new() } else { model::clock(tm.remaining_s) };
                            let tw = self.text_end(dc, self.num_strong(&time, TEXT), end, y + ROW / 2.0, ink)?;
                            self.text_in(dc, &model::timer_label(tm), TEXT, true, TEXT_X, end - tw - GAP, y + ROW / 2.0, ink)?;
                        }
                        continue;
                    }
                    // Its name and the time left (tabular: it counts every
                    // second without moving the name), the share gone as
                    // the row's bar.
                    let (name_cy, bar_top) = bar_lines(y);
                    let time = if tm.over() { String::new() } else { model::clock(tm.remaining_s) };
                    let tw = self.text_end(dc, self.num_strong(&time, TEXT), end, name_cy, ink)?;
                    self.text_in(dc, &model::timer_label(tm), TEXT, true, TEXT_X, end - tw - GAP, name_cy, ink)?;
                    self.bar(dc, TEXT_X, end, bar_top, tm.ratio(), self.tone(model::shown_tone(a)))?;
                    // The buttons under the words, then today's count at
                    // the end.
                    let cy = y + ROW + BTN / 2.0;
                    let mut x = TEXT_X;
                    for code in TIMER_BUTTONS {
                        let tg = Target::Timer(code);
                        match code {
                            1 => self.button(dc, out, tg, if tm.running { G_PAUSE } else { G_PLAY }, x, cy, true)?,
                            2 => self.button(dc, out, tg, if self.rtl { G_PREV } else { G_NEXT }, x, cy, true)?,
                            3 => {
                                // "+5" (its full words for screen readers):
                                // a capsule as wide as its words need.
                                let l = self.layout_text("+5", TEXT, false);
                                let w = (l.as_ref().map(|l| l.1).unwrap_or(0.0) + 2.0 * size::BUTTON_PAD_X).max(BTN);
                                let (bl, br) = self.mx(x, x + w);
                                let st = self.target(dc, out, tg, rect(bl, cy - BTN / 2.0, br, cy + BTN / 2.0), crate::theme::capsule(BTN))?;
                                if let Some(l) = l {
                                    self.text(dc, &l, (bl + br - l.1) / 2.0, cy, rgba(self.theme.text_of(Level::Primary, st)))?;
                                }
                                x += w - BTN;
                            }
                            _ => self.button(dc, out, tg, G_STOP, x, cy, true)?,
                        }
                        x += BTN + BTN_GAP;
                    }
                    let tw = self.layout_text(&today, SMALL, false).map(|l| l.1).unwrap_or(0.0).min(end - x - GAP);
                    self.text_in(dc, &today, SMALL, false, end - tw, end, cy, dim)?;
                    if tm.focus.as_ref().is_some_and(|v| v.no_dnd) {
                        self.text_in(dc, t("timer.no_dnd"), SMALL, false, TEXT_X, end, y + ROW + BTN + space::S2 + NOTE_H / 2.0, dim)?;
                    }
                }
                Row::Privacy(ai, g) => {
                    let Some(Activity::Privacy(uses)) = self.data.live.get(ai) else { continue };
                    let groups = model::by_app(uses);
                    let Some((app, cam, mic, scr)) = groups.get(g) else { continue };
                    let tone = if *cam { model::Tone::Green } else if *mic { model::Tone::Orange } else { model::Tone::Purple };
                    // The dot in a row glyph's box, the sentence 8 after it.
                    let (dl, dr) = self.mx(LINE, LINE + GLYPH);
                    let b = dc.CreateSolidColorBrush(&self.tone(tone), None)?;
                    dc.FillEllipse(&D2D1_ELLIPSE { point: Vector2 { X: (dl + dr) / 2.0, Y: y + h / 2.0 }, radiusX: 4.0, radiusY: 4.0 }, &b);
                    self.text_in(dc, &model::sensor_sentence(app, *cam, *mic, *scr), TEXT, false, LINE + GLYPH + GAP, end, y + h / 2.0, ink)?;
                }
                Row::Shot(ai) => {
                    let path = match self.data.live.get(ai) {
                        Some(Activity::Screenshot(s)) => s.path.clone(),
                        _ => String::new(),
                    };
                    let state = crate::shot::state(&path).unwrap_or_default();
                    let cy = y + h / 2.0;
                    // The picture at the content line (drag it out).
                    let thumb = crate::shot::thumb_for(&path);
                    let tw = thumb.as_ref().map(|(w, hh, _)| (SHOT_H * *w as f32 / (*hh).max(1) as f32).clamp(SHOT_H * 0.6, SHOT_H * 1.9)).unwrap_or(ICON);
                    let (tl, tr) = self.mx(LINE, LINE + tw);
                    let tr_rect = rect(tl, cy - SHOT_H / 2.0, tr, cy + SHOT_H / 2.0);
                    match &thumb {
                        Some((w, hh, px)) if px.len() == (*w * *hh * 4) as usize => {
                            self.picture(dc, *w, *hh, px, tr_rect)?;
                            // (Over the picture, not under it: drawn here.)
                            let st = self.target(dc, out, Target::Shot(0), tr_rect, self.theme.radii().inner)?;
                            if self.hc().is_none() {
                                self.fill_rr(dc, rgba(self.theme.fill_of(Kind::Row, st)), tr_rect, self.theme.radii().inner)?;
                            }
                        }
                        _ => self.lead(dc, G_SHOT, y, ink)?,
                    }
                    // Its buttons at the end.
                    let xs = buttons_from_end(SHOT_BUTTONS.len());
                    for ((code, g, _), x) in SHOT_BUTTONS.iter().zip(&xs) {
                        // Done: a check mark.
                        let lit = (*code == 1 && state.saved) || (*code == 2 && state.copied);
                        self.button(dc, out, Target::Shot(*code), if lit { Glyph::Check } else { *g }, *x, cy, true)?;
                    }
                    let words = if state.saved { t("island.shot.saved") } else if state.copied { t("island.shot.copied") } else { t("island.screenshot") };
                    self.text_in(dc, words, TEXT, true, LINE + tw + space::S4, xs[0] - GAP, cy, ink)?;
                }
            }
        }
        Ok(())
    }

    // ---- input

    fn dip(&self, lp: LPARAM) -> (f32, f32) {
        let m = self.m_px as f32;
        let x = (lp.0 & 0xFFFF) as i16 as f32;
        let y = ((lp.0 >> 16) & 0xFFFF) as i16 as f32;
        ((x - m) / self.ds(), (y - m) / self.ds())
    }

    /// Screen px to the card window's own DIP (dip's coordinates).
    fn screen_dip(&self, pt: POINT) -> (f32, f32) {
        ((pt.x - self.rect.left) as f32 / self.ds(), (pt.y - self.rect.top) as f32 / self.ds())
    }

    /// The target at a point of the content (its own DIP, as painted).
    fn hit(&self, x: f32, y: f32) -> Option<Target> {
        self.hits.iter().find(|(r, _)| inside(r, x, y)).map(|(_, t)| *t)
    }

    /// Still on its way out of the pill (the plate has not arrived).
    fn opening(&self) -> bool {
        !self.closing && self.plate.progress().1 > 0.5 && !self.plate.settled()
    }

    /// Where card-window DIP (`x`, `y`) is on the card as drawn now.
    fn spot_at(&self, x: f32, y: f32) -> Spot {
        spot(&self.plate.drawn(), self.opening(), x, y, |x, y| self.hit(x, y))
    }

    fn spot_on_screen(&self, pt: POINT) -> Spot {
        let (x, y) = self.screen_dip(pt);
        self.spot_at(x, y)
    }

    /// Card-window DIP x in the content's own (where it hangs now).
    fn content_x(&self, x: f32) -> f32 {
        let d = self.plate.drawn();
        d.to_content(x, d.origin.1).0
    }

    /// The hover from where the pointer is now (the card stood still, or
    /// its rows moved under a pointer that did not).
    fn rehover(&mut self) {
        if self.closing || self.dragging || self.sliding.is_some() {
            return;
        }
        let mut pt = POINT::default();
        if unsafe { GetCursorPos(&mut pt) }.is_err() {
            return;
        }
        let h = match self.spot_on_screen(pt) {
            Spot::Card(t, ..) => t,
            _ => None,
        };
        if h != self.hover {
            self.hover = h;
            self.redraw();
        }
    }

    /// Does `tg` (outside the card's borrow: it may bring a window up).
    fn action(&self, tg: Target) -> Option<(HWND, usize, isize)> {
        match tg {
            Target::Job(a, j) => {
                let Some(Activity::Progress(jobs)) = self.data.live.get(a) else { return None };
                let h = jobs.get(j)?.hwnd;
                Some((self.bar, ACT_DISMISS, h))
            }
            Target::Prev => Some((self.bar, ACT_PREV, 0)),
            Target::Toggle => Some((self.bar, ACT_TOGGLE, 0)),
            Target::Next => Some((self.bar, ACT_NEXT, 0)),
            Target::Pip => Some((self.bar, ACT_PIP, 0)),
            // The picture is dragged, not clicked.
            Target::Shot(0) => None,
            Target::Shot(k) => Some((self.bar, ACT_SHOT + k as usize, 0)),
            Target::Timer(k) => Some((self.bar, ACT_TIMER + k as usize, 0)),
            // Sliders move (keys, the pointer), they are not pressed.
            Target::Seek | Target::Volume => None,
        }
    }

    /// A screenshot thumbnail (premultiplied BGRA) filling `r`, rounded
    /// (never mirrored).
    unsafe fn picture(&self, dc: &ID2D1DeviceContext, w: u32, h: u32, px: &[u8], r: D2D_RECT_F) -> Result<()> {
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
            colorContext: std::mem::ManuallyDrop::new(None),
        };
        let bmp = dc.CreateBitmap(D2D_SIZE_U { width: w, height: h }, Some(px.as_ptr() as _), w * 4, &props)?;
        let (bw, bh) = (r.right - r.left, r.bottom - r.top);
        let k = (bw / w as f32).max(bh / h as f32);
        let bprops = D2D1_BITMAP_BRUSH_PROPERTIES1 { extendModeX: D2D1_EXTEND_MODE_CLAMP, extendModeY: D2D1_EXTEND_MODE_CLAMP, interpolationMode: D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC };
        // Filled and centered (cropped at the long side).
        let (ox, oy) = (r.left + (bw - w as f32 * k) / 2.0, r.top + (bh - h as f32 * k) / 2.0);
        let brush_props = D2D1_BRUSH_PROPERTIES { opacity: 1.0, transform: Matrix3x2 { M11: k, M12: 0.0, M21: 0.0, M22: k, M31: ox, M32: oy } };
        let b = dc.CreateBitmapBrush(&bmp, Some(&bprops), Some(&brush_props))?;
        // A picture is content: under 40 on its short side, corners 4 (3.3).
        let rad = self.theme.radii().inner;
        dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: rad, radiusY: rad }, &b);
        // No outline but in high contrast (the picture's own edge is its
        // edge).
        if self.hc().is_some() {
            self.stroke_rr(dc, self.ink(), r, rad, 1.0)?;
        }
        Ok(())
    }

    fn publish(&self) {
        if a11y::wanted(self.hwnd) {
            a11y::publish(self.hwnd, self.tree());
        }
    }

    fn tree(&self) -> Tree {
        let (ds, origin) = (self.ds(), (self.rect.left, self.rect.top));
        let px = |r: &D2D_RECT_F| a11y::px_rect(origin, ds, r.left, r.top, r.right - r.left, r.bottom - r.top);
        let mut nodes = Vec::new();
        let (rows, _, _) = layout(&self.data.live, self.vol_row());
        for (row, y, h) in rows {
            let r = rect(0.0, y, W, y + h);
            match row {
                Row::Job(a, j) => {
                    let Some(Activity::Progress(jobs)) = self.data.live.get(a) else { continue };
                    let Some(job) = jobs.get(j) else { continue };
                    let name = model::spoken(&Activity::Progress(vec![job.clone()]));
                    let n = Node::new(key_of(Target::Job(a, j)), Role::Button, name, px(&r)).value(crate::i18n::tf("control.percent", &[("p", &model::percent(job.ratio).to_string())]));
                    nodes.push(if job.hwnd != 0 { n.invoke() } else { n.enabled(false) });
                }
                Row::Media(a) => {
                    let Some(m @ Activity::Media(tr)) = self.data.live.get(a) else { continue };
                    nodes.push(Node::new("media", Role::Text, model::spoken(m), px(&r)));
                    for (tg, key, on) in [(Target::Prev, "island.prev", tr.can_prev), (Target::Toggle, if tr.playing { "island.pause" } else { "island.play" }, tr.can_toggle), (Target::Next, "island.next", tr.can_next)] {
                        let br = self.hits.iter().find(|(_, x)| *x == tg).map(|(r, _)| *r).unwrap_or(r);
                        nodes.push(Node::new(key_of(tg), Role::Button, t(key), px(&br)).invoke().enabled(on));
                    }
                    if self.data.pip {
                        let br = self.hits.iter().find(|(_, x)| *x == Target::Pip).map(|(r, _)| *r).unwrap_or(r);
                        nodes.push(Node::new(key_of(Target::Pip), Role::Button, t("pip.open"), px(&br)).invoke());
                    }
                    if let Some((p, muted)) = self.vol_level() {
                        let br = self.hits.iter().find(|(_, x)| *x == Target::Volume).map(|(r, _)| *r).unwrap_or(r);
                        nodes.push(
                            Node::new(key_of(Target::Volume), Role::Slider, t("island.volume"), px(&br))
                                .value(session::level_text(p, muted))
                                .range(Range { value: p as f64, min: 0.0, max: 100.0, step: VOL_STEP as f64 }),
                        );
                    }
                    if let (true, Some((tl, _)), Some(pos)) = (has_bar(tr), self.timeline(), self.shown_pos()) {
                        let br = self.hits.iter().find(|(_, x)| *x == Target::Seek).map(|(r, _)| *r).unwrap_or(r);
                        let (played, length) = model::time_pair(&tl, pos);
                        nodes.push(
                            Node::new(key_of(Target::Seek), Role::Slider, t("island.position"), px(&br))
                                .value(crate::i18n::tf("island.a11y.position", &[("pos", &played), ("total", &length)]))
                                .range(seek_uia(&tl, pos))
                                .enabled(tl.can_seek()),
                        );
                    }
                }
                Row::Privacy(a, g) => {
                    let Some(Activity::Privacy(u)) = self.data.live.get(a) else { continue };
                    let Some((app, c, m, s)) = model::by_app(u).get(g).cloned() else { continue };
                    nodes.push(Node::new(format!("use:{g}"), Role::Text, model::sensor_sentence(&app, c, m, s), px(&r)));
                }
                Row::Battery(a) | Row::Timer(a) | Row::Shot(a) => {
                    let Some(act) = self.data.live.get(a) else { continue };
                    nodes.push(Node::new(format!("row:{a}"), Role::Text, model::spoken(act), px(&r)));
                }
            }
        }
        // The focus timer's buttons.
        let running = self.data.live.iter().any(|a| matches!(a, Activity::Timer(tm) if tm.running));
        for (r, tg) in self.hits.iter().filter(|(_, tg)| matches!(tg, Target::Timer(_))) {
            let Target::Timer(k) = *tg else { continue };
            nodes.push(Node::new(key_of(*tg), Role::Button, timer_button_text(k, running), px(r)).invoke());
        }
        // The screenshot's picture and buttons.
        for (r, tg) in self.hits.iter().filter(|(_, tg)| matches!(tg, Target::Shot(_))) {
            let Target::Shot(k) = *tg else { continue };
            let node = match SHOT_BUTTONS.iter().find(|b| b.0 == k) {
                Some((_, _, label)) => Node::new(key_of(*tg), Role::Button, t(label), px(r)).invoke(),
                None => Node::new(key_of(*tg), Role::Text, t("island.screenshot"), px(r)).help(t("island.shot.drag")),
            };
            nodes.push(node);
        }
        Tree {
            role: Role::Group,
            name: t("settings.topbar.island").into(),
            nodes,
            focus: self.focus.map(key_of),
            focusable: self.pinned,
            active: self.pinned,
            focus_in: None,
        }
    }
}

/// Runs a target's action with the card unborrowed.
fn run(act: Option<(HWND, usize, isize)>) {
    let Some((bar, code, hwnd)) = act else { return };
    if hwnd != 0 {
        crate::dock::bring_to_front(HWND(hwnd as *mut _));
    }
    tell(bar, code);
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // A card going back into the pill, and the window of one already
    // gone, take nothing: the pointer goes through, and nothing they are
    // still sent reaches the card that is out now.
    if with_card(|c| c.hwnd == hwnd && !c.closing) == Some(false) {
        return match msg {
            WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            WM_DESTROY => {
                a11y::forget(hwnd);
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_MOUSEMOVE | WM_MOUSELEAVE_MSG | WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_LBUTTONUP | WM_MOUSEWHEEL | WM_TIMER | WM_CARD_VOLUME | WM_KEYDOWN
            | WM_SYSKEYDOWN | WM_ACTIVATE | WM_CAPTURECHANGED | WM_CLOSE => LRESULT(0),
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        };
    }
    match msg {
        // A closed card fading out lets the pointer through.
        WM_NCHITTEST if crate::motion::retired(hwnd) => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEMOVE => {
            let drag = with_card(|c| {
                if !c.tracking {
                    let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: c.hwnd, dwHoverTime: 0 };
                    let _ = TrackMouseEvent(&mut tme);
                    c.tracking = true;
                }
                if c.dragging {
                    return false;
                }
                // A held slider follows the pointer (outside the card too).
                if let Some(tg) = c.sliding {
                    let (x, _) = c.dip(lp);
                    c.slide_to(tg, c.content_x(x));
                    c.redraw();
                    return false;
                }
                // The screenshot's picture pressed and moved: dragged out.
                if c.pressed == Some(Target::Shot(0)) && (wp.0 & 0x0001) != 0 {
                    if let Some(at) = c.press_at {
                        let (x, y) = ((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32);
                        if (x - at.x).abs() >= GetSystemMetrics(SM_CXDRAG).max(2) || (y - at.y).abs() >= GetSystemMetrics(SM_CYDRAG).max(2) {
                            c.dragging = true;
                            return true;
                        }
                    }
                }
                // (What is seen is what is under it: during the morph too.)
                let (x, y) = c.dip(lp);
                let h = match c.spot_at(x, y) {
                    Spot::Card(t, ..) => t,
                    _ => None,
                };
                if h != c.hover {
                    c.hover = h;
                    c.keyboard = false;
                    c.redraw();
                }
                false
            });
            if drag == Some(true) {
                crate::shot::drag_out();
                let bar = with_card(|c| {
                    c.dragging = false;
                    c.pressed = None;
                    c.press_at = None;
                    c.bar
                });
                // Done with it: the card goes (the island keeps the pill).
                if let Some(b) = bar {
                    tell(b, ACT_DISMISS);
                }
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE_MSG => {
            let bar = with_card(|c| {
                c.tracking = false;
                if c.dragging || c.sliding.is_some() {
                    return None;
                }
                c.hover = None;
                c.redraw();
                (!c.pinned).then_some(c.bar)
            })
            .flatten();
            if let Some(b) = bar {
                tell(b, ACT_LEAVE);
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            // (Dismiss with the bar, or hold the mouse for a slider.)
            let out = with_card(|c| {
                let (x, y) = c.dip(lp);
                let at = c.spot_at(x, y);
                // Any press hides the keyboard's ring (crate::input).
                if std::mem::replace(&mut c.keyboard, false) {
                    c.redraw();
                }
                let h = match press_do(at, c.pinned, msg == WM_LBUTTONDOWN) {
                    // Outside (the pill included, held by the capture), or
                    // the pill pressed again while it opens.
                    PressDo::Dismiss => return (Some(c.bar), false),
                    PressDo::Pin => {
                        tell(c.bar, ACT_PIN);
                        return (None, false);
                    }
                    PressDo::Nothing => return (None, false),
                    PressDo::Press(h) => h,
                };
                let Spot::Card(_, cx, _) = at else { return (None, false) };
                match h {
                    // A slider: held, and it goes to the pointer now.
                    Some(tg) if tg.slider() => {
                        c.sliding = Some(tg);
                        c.focus = Some(tg);
                        c.keyboard = false;
                        c.slide_to(tg, cx);
                        c.redraw();
                        // A card out by hover holds no capture of its own.
                        return (None, !c.pinned);
                    }
                    h => c.pressed = h,
                }
                c.press_at = Some(POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 });
                c.redraw();
                // A click on the card itself, out by hovering: it stays
                // (the pill it stands on cannot be clicked any more).
                if !c.pinned && c.pressed.is_none() {
                    tell(c.bar, ACT_PIN);
                }
                (None, false)
            });
            match out {
                Some((Some(b), _)) => tell(b, ACT_DISMISS),
                Some((None, true)) => {
                    SetCapture(hwnd);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            // (The action, or what letting go of a slider leaves to do.)
            let out = with_card(|c| {
                if c.sliding.is_some() {
                    let released = c.let_go(true);
                    c.redraw();
                    c.publish();
                    c.sync_tick();
                    return (None, released);
                }
                let (x, y) = c.dip(lp);
                let p = c.pressed.take();
                c.redraw();
                // (A hover card's capture is only ever a slider's; one let
                // go of already is released here all the same.)
                let stray = Released { hwnd: Some(c.hwnd), capture: !c.pinned, leave: None };
                let under = match c.spot_at(x, y) {
                    Spot::Card(t, ..) => t,
                    _ => None,
                };
                (p.filter(|p| under == Some(*p)).and_then(|p| c.action(p)), stray)
            });
            let Some((act, released)) = out else { return LRESULT(0) };
            released.run();
            run(act);
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            // Over the volume: VOL_STEP a notch (fractions add up).
            let delta = ((wp.0 >> 16) & 0xFFFF) as i16 as i32;
            with_card(|c| {
                if c.hover != Some(Target::Volume) && c.sliding != Some(Target::Volume) {
                    c.wheel_acc = 0;
                    return;
                }
                let Some((p, _)) = c.vol_level() else { return };
                if (c.wheel_acc > 0) != (delta > 0) {
                    c.wheel_acc = 0;
                }
                c.wheel_acc += delta;
                let notches = c.wheel_acc / WHEEL_NOTCH;
                c.wheel_acc -= notches * WHEEL_NOTCH;
                if notches != 0 {
                    c.set_volume(p as i32 + notches * VOL_STEP);
                    c.redraw();
                    c.publish();
                }
            });
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TICK_ID => {
            with_card(|c| {
                c.settle_hold();
                c.redraw();
                c.publish();
                c.sync_tick();
            });
            LRESULT(0)
        }
        WM_CARD_VOLUME => {
            let released = with_card(|c| {
                let r = c.take_volume();
                // (An app without a volume: its row goes.)
                c.relayout();
                c.redraw();
                c.rehover();
                c.publish();
                r
            });
            if let Some(r) = released {
                r.run();
            }
            LRESULT(0)
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let vk = wp.0 as u16;
            let act = with_card(|c| c.on_key(vk))
            .flatten();
            run(act);
            LRESULT(0)
        }
        WM_ACTIVATE if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            if let Some(b) = with_card(|c| (c.pinned && !c.dragging).then_some(c.bar)).flatten() {
                tell(b, ACT_DISMISS);
            }
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            let now = GetCapture();
            let out = with_card(|c| {
                if now == c.hwnd {
                    return (None, Released::default());
                }
                let mut released = Released::default();
                if c.sliding.is_some() && !c.pinned {
                    // A slider held by hover lost the mouse: let go of it
                    // where it is (no seek: the button did not come up),
                    // and see whether the pointer left meanwhile.
                    released = c.let_go(false);
                    c.redraw();
                }
                ((c.pinned && !c.dragging).then_some(c.bar), released)
            });
            if let Some((dismiss, released)) = out {
                released.run();
                if let Some(b) = dismiss {
                    tell(b, ACT_DISMISS);
                }
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            if let Some(b) = with_card(|c| c.bar) {
                tell(b, ACT_DISMISS);
            }
            LRESULT(0)
        }
        WM_GETOBJECT => {
            let answer = a11y::on_getobject(hwnd, wp, lp, || {
                with_card(|c| c.publish());
            });
            answer.unwrap_or_else(|| DefWindowProcW(hwnd, msg, wp, lp))
        }
        m if m == a11y::wm_action() => {
            for (key, act) in a11y::take(hwnd) {
                let todo = with_card(|c| match act {
                    Act::Refresh => {
                        c.publish();
                        None
                    }
                    Act::Invoke => c.order().into_iter().find(|tg| key_of(*tg) == key).and_then(|tg| c.action(tg)),
                    Act::Focus => {
                        // The focus moves; the ring as it was, or with
                        // Windows' keyboard cues always on (crate::input).
                        c.focus = c.order().into_iter().find(|tg| key_of(*tg) == key);
                        c.keyboard = crate::input::reader_focus(c.keyboard);
                        c.redraw();
                        c.publish();
                        None
                    }
                    // A slider set to a number (RangeValue): seconds played,
                    // or percent.
                    Act::SetValue => {
                        let v = a11y::take_value(hwnd, &key).and_then(|s| s.trim().parse::<f64>().ok()).filter(|v| v.is_finite());
                        match (c.order().into_iter().find(|tg| key_of(*tg) == key), v) {
                            (Some(Target::Seek), Some(v)) => {
                                if let Some((tl, _)) = c.timeline() {
                                    c.seek(tl.start_ms.saturating_add((v * 1000.0).round() as i64));
                                }
                            }
                            (Some(Target::Volume), Some(v)) => c.set_volume(v.round() as i32),
                            _ => {}
                        }
                        c.redraw();
                        c.publish();
                        c.sync_tick();
                        None
                    }
                    _ => None,
                })
                .flatten();
                run(todo);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            a11y::forget(hwnd);
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// The card's picture for the promo film (tests only).
#[cfg(test)]
#[path = "card_promo.rs"]
pub(super) mod promo;

/// Pictures for the design audit (tests only).
#[cfg(test)]
#[path = "card_harmony.rs"]
mod harmony;
#[cfg(test)]
pub(in crate::topbar) use harmony::paint_site_island;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topbar::island::model::{Job, Sensor, Track, Use};

    fn job(k: &str, h: isize) -> Job {
        Job { key: k.into(), name: k.into(), files: false, ratio: 0.5, hwnd: h }
    }

    #[test]
    fn rows_stack_with_separators_between_activities() {
        let live = vec![
            Activity::Progress(vec![job("a", 1), job("b", 0)]),
            Activity::Media(Track { title: "t".into(), playing: true, can_toggle: true, can_next: true, ..Default::default() }),
            Activity::Privacy(vec![Use { sensor: Sensor::Camera, id: "z".into(), app: "Zoom".into() }, Use { sensor: Sensor::Microphone, id: "t".into(), app: "Teams".into() }]),
        ];
        let (rows, seps, h) = layout(&live, false);
        let kinds: Vec<Row> = rows.iter().map(|r| r.0).collect();
        assert_eq!(kinds, [Row::Job(0, 0), Row::Job(0, 1), Row::Media(1), Row::Privacy(2, 0), Row::Privacy(2, 1)]);
        assert_eq!(seps.len(), 2);
        // Rows never overlap and the card holds them all.
        for w in rows.windows(2) {
            assert!(w[0].1 + w[0].2 <= w[1].1);
        }
        let last = rows.last().unwrap();
        assert!(last.1 + last.2 <= h);
        // Only apps with a window and the media buttons it allows take focus.
        let order = targets(&live, false, false);
        assert_eq!(order, [Target::Job(0, 0), Target::Toggle, Target::Next]);
        assert_eq!(step_focus(&order, None, 1), Some(Target::Job(0, 0)));
        assert_eq!(step_focus(&order, None, -1), Some(Target::Next));
        assert_eq!(step_focus(&order, Some(Target::Next), 1), Some(Target::Job(0, 0)), "wraps");
        assert_eq!(step_focus(&order, Some(Target::Prev), 1), Some(Target::Job(0, 0)), "gone: from the start");
        assert_eq!(step_focus(&[], None, 1), None);
    }

    #[test]
    fn the_card_keeps_the_panels_two_lines() {
        // Design system v2.1, 3.3: the vessel line 8 in, the content line
        // 12 further; a two-line row's words 28 + 12 after that.
        assert_eq!((PAD, LINE, TEXT_X), (8.0, 20.0, 60.0));
        // The mock-up's card: a song with its bar and volume, a hairline
        // slot, a copy: 8 + 132 + 8 + 48 + 8.
        let at = std::time::Instant::now();
        let tl = model::Timeline { start_ms: 0, end_ms: 200_000, pos_ms: 0, at, rate: 1.0, seekable: true, seek_min_ms: 0, seek_max_ms: 0 };
        let live = vec![Activity::Media(Track { title: "t".into(), timeline: Some(tl), ..Default::default() }), Activity::Progress(vec![job("a", 0)])];
        let (rows, seps, h) = layout(&live, true);
        assert_eq!(rows, [(Row::Media(0), 8.0, 132.0), (Row::Job(1, 0), 148.0, 48.0)]);
        assert_eq!((seps, h), (vec![144.0], 204.0));
        // What is playing, top to bottom: 12, the art 56, 8, the times 16,
        // 8, the volume 20, 12.
        assert_eq!(media_height(true, true), 12.0 + 56.0 + 8.0 + 16.0 + 8.0 + 20.0 + 12.0);
        assert_eq!(media_height(false, true), 108.0);
        assert_eq!(media_height(true, false), 104.0);
        assert_eq!(media_height(false, false), 80.0);
        // Every length on the scale (it had 5, 10, 15, 17, 22, 29, 33).
        let scale = crate::theme::metrics::space::SCALE;
        for v in [PAD, SEP, GAP, BTN_GAP, size::CARD_PAD, TEXT_X - LINE - ICON] {
            assert!(scale.contains(&v), "{v}");
        }
        for v in [ROW, ONE, ICON, ART, BTN, TIME_H, VOL_H, SHOT_H, NOTE_H, TRACK_H, GLYPH, GLYPH_L] {
            assert!((v / 4.0).fract() == 0.0, "{v}");
        }
        // Glyphs: 16 in buttons and rows, 24 in a two-line row's box.
        assert_eq!((GLYPH, GLYPH_L, KNOB_R * 2.0, TRACK_H), (16.0, 24.0, 16.0, 4.0));
        // A two-line row: the name and its bar (18 + 8 + 4) centered, or
        // the name and a note (18 + 16).
        assert_eq!(bar_lines(100.0), (118.0, 135.0));
        assert_eq!(note_lines(100.0), (116.0, 133.0));
        // Glyph buttons end on the content line's mirror, 4 apart.
        assert_eq!(buttons_from_end(3), [228.0, 260.0, 292.0]);
        assert_eq!(buttons_from_end(4)[0], 196.0);
        assert_eq!(buttons_from_end(3)[2] + BTN, W - LINE);
    }

    #[test]
    fn the_focus_timer_row_has_its_buttons() {
        let _l = crate::i18n::test_lock("en");
        let v = crate::focus_timer::View { phase: 0, left_ms: 60_000, total_ms: 120_000, running: true, deadline: 1, today: 1, no_dnd: false };
        let live = vec![Activity::Timer(model::Timer::of_focus(v.clone()))];
        let (rows, _, h) = layout(&live, false);
        // Its two lines, its buttons under them, 4 below.
        assert_eq!(rows, [(Row::Timer(0), PAD, 80.0)]);
        assert_eq!(h, 96.0);
        assert_eq!(targets(&live, false, false), [Target::Timer(1), Target::Timer(2), Target::Timer(3), Target::Timer(4)]);
        // The note line when Do not disturb is not offered here.
        let note = vec![Activity::Timer(model::Timer::of_focus(crate::focus_timer::View { no_dnd: true, ..v.clone() }))];
        assert_eq!(layout(&note, false).0[0].2, 100.0);
        // Over: a two-line row, nothing to press.
        let over = vec![Activity::Timer(model::Timer::of_focus(crate::focus_timer::View { phase: 2, ..v }))];
        assert_eq!(layout(&over, false).0[0].2, ROW);
        assert!(targets(&over, false, false).is_empty());
        assert_eq!(timer_button_text(1, true), "Pause");
        assert_eq!(timer_button_text(1, false), "Resume");
        assert_eq!(key_of(Target::Timer(3)), "timer:3");
    }

    #[test]
    fn what_is_playing_has_its_position_bar_and_volume() {
        let at = std::time::Instant::now();
        let tl = model::Timeline { start_ms: 0, end_ms: 200_000, pos_ms: 0, at, rate: 1.0, seekable: true, seek_min_ms: 0, seek_max_ms: 0 };
        let tr = Track { title: "t".into(), playing: true, can_toggle: true, timeline: Some(tl), ..Default::default() };
        let live = vec![Activity::Media(tr.clone())];
        assert_eq!(layout(&live, false).0[0].2, media_height(true, false), "the bar under it");
        assert_eq!(layout(&live, true).0[0].2, media_height(true, true), "and the volume under that");
        // As they stand: the buttons, the bar, then the volume (when its
        // app has one).
        assert_eq!(targets(&live, true, false), [Target::Toggle, Target::Seek, Target::Volume]);
        assert_eq!(targets(&live, false, false), [Target::Toggle, Target::Seek]);
        // A bar the app will not move is read only: not in the order.
        let fixed = vec![Activity::Media(Track { timeline: Some(model::Timeline { seekable: false, ..tl }), ..tr.clone() })];
        assert_eq!(layout(&fixed, false).0[0].2, media_height(true, false));
        assert_eq!(targets(&fixed, false, false), [Target::Toggle]);
        // Nothing left of the seek window: read only too.
        let shut = vec![Activity::Media(Track { timeline: Some(model::Timeline { seek_min_ms: 9_000, seek_max_ms: 9_000, ..tl }), ..tr.clone() })];
        assert_eq!(targets(&shut, false, false), [Target::Toggle]);
        // No timeline (EndTime 0): no bar at all.
        let none = vec![Activity::Media(Track { timeline: Some(model::Timeline { end_ms: 0, ..tl }), ..tr.clone() })];
        assert_eq!(layout(&none, false).0[0].2, media_height(false, false));
        assert_eq!(targets(&none, false, false), [Target::Toggle]);
        assert_eq!(layout(&[Activity::Media(Track { timeline: None, ..tr })], false).0[0].2, media_height(false, false));
        assert_eq!((key_of(Target::Seek), key_of(Target::Volume)), ("seek".to_string(), "volume".to_string()));
        // The PiP button before the media buttons, when its window is found.
        assert_eq!(targets(&live, true, true), [Target::Pip, Target::Toggle, Target::Seek, Target::Volume]);
        assert_eq!(key_of(Target::Pip), "pip");
        assert!(!Target::Pip.slider());
        assert!(Target::Seek.slider() && Target::Volume.slider() && !Target::Toggle.slider());
        // Distinct from the bar's messages (it is posted to the card only,
        // but kept out of the way of the shared ones).
        assert_ne!(WM_CARD_VOLUME, WM_ISLAND_ACT);
        assert_ne!(WM_CARD_VOLUME, super::super::WM_ISLAND_MEDIA);
    }

    #[test]
    fn a_seek_hold_runs_out_even_while_paused() {
        let ms = Duration::from_millis;
        assert_eq!(next_tick(None, None), None, "paused, nothing held: no timer");
        assert_eq!(next_tick(Some(700), None), Some(715));
        assert_eq!(next_tick(None, Some(ms(2500))), Some(2515), "paused with a hold: it still ends");
        assert_eq!(next_tick(Some(700), Some(ms(300))), Some(315), "whichever comes first");
        assert_eq!(next_tick(Some(300), Some(ms(2000))), Some(315));
        assert_eq!(next_tick(None, Some(ms(0))), Some(16), "never a busy loop");
    }

    #[test]
    fn only_answers_since_the_newest_change_show() {
        let a = |seq: u64, p: u8| session::AppVolume { app: "Chrome".into(), level: Some((p, false)), seq };
        // 50 % -> two key presses: 55 % (seq 7), 60 % (seq 8).
        assert!(!volume_answer_counts(&a(7, 55), "Chrome", 8), "the first press's answer does not undo the second");
        assert!(volume_answer_counts(&a(8, 60), "Chrome", 8));
        assert!(volume_answer_counts(&a(9, 60), "Chrome", 8), "a later read");
        assert!(!volume_answer_counts(&a(9, 60), "Spotify.exe", 8), "another app's");
        assert!(!volume_answer_counts(&a(9, 60), "", 0));
    }

    #[test]
    fn screen_readers_get_the_seek_window_in_seconds() {
        let at = std::time::Instant::now();
        let base = model::Timeline { start_ms: 0, end_ms: 10_500, pos_ms: 0, at, rate: 1.0, seekable: true, seek_min_ms: 0, seek_max_ms: 0 };
        let r = seek_uia(&base, 1_500);
        assert_eq!((r.value, r.min, r.max, r.step), (1.5, 0.0, 10.5, 5.0), "fractions kept");
        let clip = model::Timeline { end_ms: 500, ..base };
        assert_eq!(seek_uia(&clip, 250).max, 0.5, "a short clip still has a range");
        // A live window: only where a seek may go, the value inside it.
        let live = model::Timeline { start_ms: 0, end_ms: 3_600_000, seek_min_ms: 3_000_000, seek_max_ms: 3_590_000, ..base };
        let r = seek_uia(&live, 3_599_000);
        assert_eq!((r.value, r.min, r.max), (3590.0, 3000.0, 3590.0));
        // From the timeline's start; a bar that takes no seek spans the track.
        let fixed = model::Timeline { start_ms: 10_000, end_ms: 70_000, seekable: false, ..base };
        let r = seek_uia(&fixed, 40_000);
        assert_eq!((r.value, r.min, r.max), (30.0, 0.0, 60.0));
    }

    #[test]
    fn the_card_stands_on_the_pill() {
        // A 32 px bar at the top, its work area under it; the pill 2 in.
        let work = RECT { left: 0, top: 32, right: 1920, bottom: 1040 };
        let pill = RECT { left: 900, top: 2, right: 1020, bottom: 30 };
        let (r, fit) = place(pill, work, 1.0, 200.0, false);
        assert_eq!(fit, 1.0);
        // Its top edge is the pill's, its middle the pill's: it grows down
        // out of it (the mock-up's `top: 2px`).
        assert_eq!((r.top, r.bottom), (2, 202));
        assert_eq!((r.left + r.right) / 2, 960);
        // Near an edge: kept EDGE_GAP inside the work area.
        let (r, _) = place(RECT { left: 0, top: 2, right: 40, bottom: 30 }, work, 1.0, 200.0, false);
        assert_eq!(r.left, 8);
        let (r, _) = place(RECT { left: 1880, top: 2, right: 1920, bottom: 30 }, work, 1.5, 200.0, false);
        assert_eq!(r.right, 1920 - 12);
        // A short screen: shrunk to the room from the pill down.
        let (s, fit) = place(pill, RECT { left: 0, top: 32, right: 800, bottom: 100 }, 1.0, 200.0, false);
        assert!(fit < 1.0 && s.top == 2 && s.bottom <= 100 - 8, "{s:?}");
    }

    #[test]
    fn over_a_bottom_bar_the_card_rises_from_the_pill() {
        // The bar at the bottom reserves 32 px; the pill 2 in from its edges.
        let work = RECT { left: 0, top: 0, right: 1920, bottom: 1048 };
        let pill = RECT { left: 900, top: 1050, right: 1020, bottom: 1078 };
        let (r, fit) = place(pill, work, 1.0, 200.0, true);
        assert_eq!(fit, 1.0);
        assert_eq!((r.left + r.right) / 2, 960);
        // Its bottom edge is the pill's: it grows upward.
        assert_eq!((r.bottom, r.top), (1078, 878));
        // A taller card (music) grows further up, still inside.
        let (t, _) = place(pill, work, 1.0, 600.0, true);
        assert_eq!(t.bottom, 1078);
        assert!(t.top >= 8);
        // A short screen: shrunk, inside.
        let (s, fit) = place(pill, RECT { top: 950, ..work }, 1.0, 200.0, true);
        assert!(fit < 1.0 && s.top >= 950 && s.bottom == 1078, "{s:?}");
    }

    #[test]
    fn the_plate_starts_exactly_on_the_pill() {
        // The card of the test above: the pill is its top middle.
        let pill = RECT { left: 900, top: 2, right: 1020, bottom: 30 };
        let card = RECT { left: 790, top: 2, right: 1130, bottom: 202 };
        let s = pill_shape(pill, 1.0, card, 1.0);
        assert_eq!((s.x, s.y, s.w, s.h, s.r), (110.0, 0.0, 120.0, 28.0, 14.0));
        // At 150 % the same pill in the card's DIP.
        let s = pill_shape(RECT { left: 1350, top: 3, right: 1530, bottom: 45 }, 1.0, RECT { left: 1185, top: 3, right: 1695, bottom: 303 }, 1.5);
        assert_eq!((s.x, s.y, s.w, s.h, s.r), (110.0, 0.0, 120.0, 28.0, 14.0));
        // Under the pointer it is 1.02 about its middle: the plate starts
        // on what is shown.
        let s = pill_shape(pill, 1.02, card, 1.0);
        assert!((s.w - 122.4).abs() < 1e-3 && (s.x - 108.8).abs() < 1e-3 && (s.y + 0.28).abs() < 1e-3 && (s.r - s.h / 2.0).abs() < 1e-6, "{s:?}");
        // Over a bar at the bottom the pill is the card's bottom middle.
        let s = pill_shape(RECT { left: 900, top: 1050, right: 1020, bottom: 1078 }, 1.0, RECT { left: 790, top: 878, right: 1130, bottom: 1078 }, 1.0);
        assert_eq!((s.y, s.y + s.h), (172.0, 200.0));
        // The window holds the card's shadow, and a pill that reaches past
        // a card the screen's edge pushed aside.
        assert_eq!(margin_px(pill, card, 1.0), 48);
        assert_eq!(margin_px(pill, card, 1.5), 72);
        let aside = RECT { left: 8, top: 2, right: 348, bottom: 202 };
        let far = RECT { left: 300, top: 2, right: 420, bottom: 30 };
        assert!(margin_px(far, aside, 1.0) >= 72 + 8, "{}", margin_px(far, aside, 1.0));
    }

    /// The volume near the card's bottom and play / pause near its top, as
    /// painted (the content's own DIP), on a card 200 tall.
    const H: f32 = 200.0;
    fn painted() -> Vec<(D2D_RECT_F, Target)> {
        vec![(rect(LINE, H - 40.0, W - LINE, H - 20.0), Target::Volume), (rect(240.0, 40.0, 280.0, 72.0), Target::Toggle)]
    }
    fn mid(r: &D2D_RECT_F) -> (f32, f32) {
        ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0)
    }

    #[test]
    fn what_is_seen_is_what_is_pressed_during_the_morph() {
        use morph::{card_origin, drawn_at, plate_at, Shape};
        let hits = painted();
        let hit = |x: f32, y: f32| hits.iter().find(|(r, _)| inside(r, x, y)).map(|(_, t)| *t);
        let card = Shape { x: 0.0, y: 0.0, w: W, h: H, r: 20.0 };
        let pill = |x: f32, y: f32| Shape { x, y, w: 120.0, h: 28.0, r: 14.0 };
        // A bar at the top, a bar at the bottom, a card the screen's edge
        // pushed aside (its pill reaches past the card's left).
        let cases = [("top", pill(110.0, 0.0), false), ("bottom", pill(110.0, H - 28.0), true), ("edge", pill(-60.0, 0.0), false)];
        for (name, pill, up) in cases {
            for p in [0.35f32, 0.7] {
                let d = drawn_at(pill, card, p, up);
                // Where the compositor hangs the content (morph.rs: the
                // same expression), and the plate that clips it.
                assert_eq!(d.origin, card_origin(plate_at(pill, card, p), card, up), "{name} {p}");
                for (r, t) in &hits {
                    let (cx, cy) = mid(r);
                    let shown = (cx * d.k + d.origin.0, cy * d.k + d.origin.1);
                    let at = spot(&d, true, shown.0, shown.1, hit);
                    if p < 0.5 {
                        // Not there yet (0.35: none of the card's content
                        // shows): nothing in it is pressed, wherever.
                        assert!(!matches!(at, Spot::Card(Some(_), ..)), "{name} {p} {t:?}: {at:?}");
                        assert!(!matches!(spot(&d, true, cx, cy, hit), Spot::Card(Some(_), ..)), "{name} {p} {t:?}");
                    } else if morph::on_shape(d.plate, shown.0, shown.1) && !inside(&rect(pill.x, pill.y, pill.x + pill.w, pill.y + pill.h), shown.0, shown.1) {
                        // Seen: pressed where it is drawn, at its own point.
                        match at {
                            Spot::Card(Some(u), x, y) => assert!(u == *t && (x - cx).abs() < 1e-3 && (y - cy).abs() < 1e-3, "{name} {p}"),
                            other => panic!("{name} {p} {t:?}: {other:?}"),
                        }
                    } else {
                        // Still under the plate's clip: not pressed.
                        assert!(!matches!(at, Spot::Card(Some(_), ..)), "{name} {p} {t:?}: {at:?}");
                    }
                }
            }
        }
        // Top bar at 0.7: the plate is 148 tall, the volume (160..180) is
        // not uncovered yet: where it will be is off the plate.
        let d = drawn_at(pill(110.0, 0.0), card, 0.7, false);
        let (vx, vy) = mid(&hits[0].0);
        assert_eq!(spot(&d, true, vx, vy, hit), Spot::Outside);
        // Over a bar at the bottom at 0.7 the plate's top is 52 down: above
        // it nothing is pressed; play / pause, where it shows (its content
        // drawn smaller, morph::content_scale), is.
        let d = drawn_at(pill(110.0, H - 28.0), card, 0.7, true);
        let (tx, ty) = mid(&hits[1].0);
        assert!(matches!(spot(&d, true, tx * d.k + d.origin.0, d.plate.y - 2.0, hit), Spot::Outside));
        assert!(matches!(spot(&d, true, tx * d.k + d.origin.0, ty * d.k + d.origin.1, hit), Spot::Card(Some(Target::Toggle), ..)));
        // Pushed aside at 0.7 the content still slides in with the plate,
        // never wider than it: play / pause is not where it ends up yet.
        let d = drawn_at(pill(-60.0, 0.0), card, 0.7, false);
        assert!(d.k < 1.0 && (d.origin.0 - d.plate.x).abs() < 1e-3 && d.origin.0 < 0.0, "{d:?}");
        assert!(!matches!(spot(&d, true, tx, ty, hit), Spot::Card(Some(Target::Toggle), ..)));
        assert!(matches!(spot(&d, true, tx * d.k + d.origin.0, ty * d.k + d.origin.1, hit), Spot::Card(Some(Target::Toggle), ..)));
        // Arrived: the card is where it stands, the pill's place is the card.
        let d = drawn_at(pill(110.0, 0.0), card, 1.0, false);
        assert_eq!(d.origin, (0.0, 0.0));
        assert_eq!(spot(&d, false, vx, vy, hit), Spot::Card(Some(Target::Volume), vx, vy));
        assert!(matches!(spot(&d, false, 170.0, 14.0, hit), Spot::Card(None, ..)));
        // Off the card, and in its cut corner: outside.
        assert_eq!(spot(&d, false, W + 1.0, 50.0, hit), Spot::Outside);
        assert_eq!(spot(&d, false, 1.0, 1.0, hit), Spot::Outside);
    }

    #[test]
    fn the_pill_pressed_again_turns_the_morph_round() {
        use crate::topbar::island::model::{step, Event, Phase};
        use morph::{drawn_at, Shape};
        let card = Shape { x: 0.0, y: 0.0, w: W, h: H, r: 20.0 };
        let pill = Shape { x: 110.0, y: 0.0, w: 120.0, h: 28.0, r: 14.0 };
        let hits = painted();
        let hit = |x: f32, y: f32| hits.iter().find(|(r, _)| inside(r, x, y)).map(|(_, t)| *t);
        // While it opens, the pill's place is the pill (under the plate,
        // which grows out of it) at every point of the way.
        for p in [0.0f32, 0.2, 0.35, 0.7, 0.95] {
            assert_eq!(spot(&drawn_at(pill, card, p, false), true, 170.0, 14.0, hit), Spot::Pill, "{p}");
        }
        // Out by a click: pressed again, it goes back (ACT_DISMISS); the
        // island's Dismiss is the pill again, and close_into sends the
        // morph from where it is to 0 (Card::fold; the spring turning round
        // is morph.rs's the_morph_turns_round_where_it_is, the whole path on
        // a real card card_harmony.rs's island_reclick_turns_the_morph_round).
        assert_eq!(press_do(Spot::Pill, true, true), PressDo::Dismiss);
        assert_eq!(step(Phase::Expanded { pinned: true }, Event::Dismiss), Phase::Pill);
        // Out by hovering: a press keeps it (ACT_PIN), as on the card.
        assert_eq!(press_do(Spot::Pill, false, true), PressDo::Pin);
        assert_eq!(step(Phase::Expanded { pinned: false }, Event::Click), Phase::Expanded { pinned: true });
        // Other buttons on the pill or the card do nothing; outside
        // dismisses as ever; the card's targets are pressed.
        assert_eq!(press_do(Spot::Pill, true, false), PressDo::Nothing);
        assert_eq!(press_do(Spot::Card(Some(Target::Toggle), 1.0, 1.0), true, false), PressDo::Nothing);
        assert_eq!(press_do(Spot::Outside, false, false), PressDo::Dismiss);
        assert_eq!(press_do(Spot::Card(Some(Target::Volume), 1.0, 1.0), false, true), PressDo::Press(Some(Target::Volume)));
        assert_eq!(press_do(Spot::Card(None, 1.0, 1.0), true, true), PressDo::Press(None));
    }

    #[test]
    fn the_wash_follows_its_target_when_the_rows_move() {
        let at = std::time::Instant::now();
        let tl = model::Timeline { start_ms: 0, end_ms: 200_000, pos_ms: 0, at, rate: 1.0, seekable: true, seek_min_ms: 0, seek_max_ms: 0 };
        let live = vec![Activity::Media(Track { title: "t".into(), timeline: Some(tl), ..Default::default() }), Activity::Progress(vec![job("a", 1)])];
        // The copy's row under what plays: the volume's answer folds its
        // row (no audio session), and the copy moves up; same target.
        let row = |volume: bool| {
            let (rows, _, _) = layout(&live, volume);
            let &(_, y, h) = rows.iter().find(|r| r.0 == Row::Job(1, 0)).unwrap();
            WashOn { t: Target::Job(1, 0), down: false, r: rect(PAD, y, W - PAD, y + h), rad: 8.0 }
        };
        let (before, after) = (row(true), row(false));
        assert!(after.r.top < before.r.top, "the row moved up");
        assert_eq!(wash_step(Some(before), Some(after)), WashStep::Same { press: false, moved: true });
        // Nothing changed: nothing to do.
        assert_eq!(wash_step(Some(after), Some(after)), WashStep::Stay);
        // Pressed where it is, and pressed as it moves.
        assert_eq!(wash_step(Some(after), Some(WashOn { down: true, ..after })), WashStep::Same { press: true, moved: false });
        assert_eq!(wash_step(Some(before), Some(WashOn { down: true, ..after })), WashStep::Same { press: true, moved: true });
        // Another radius on the same box is a move too.
        assert_eq!(wash_step(Some(after), Some(WashOn { rad: 4.0, ..after })), WashStep::Same { press: false, moved: true });
        // Another target, or none: the other wash.
        assert_eq!(wash_step(Some(after), Some(WashOn { t: Target::Toggle, ..after })), WashStep::Other);
        assert_eq!(wash_step(None, Some(after)), WashStep::Other);
        assert_eq!(wash_step(Some(after), None), WashStep::Other);
        assert_eq!(wash_step(None, None), WashStep::Stay);
    }
}
