//! Dragging (polish plan §4): reorder pinned apps, drag a running app into
//! the pinned area to pin it, drag a pinned icon off the dock to remove it,
//! drop files on an app to open them with it, drop shortcuts / programs on
//! the dock to pin them where they are dropped.
//!
//! Removing uses a dwell threshold: a pinned icon pulled REMOVE_DIST off
//! the bar (away from the screen edge) dims, and after REMOVE_ARM_MS shows
//! "Remove"; released then, it fades out. Released before that, or
//! dragged off to the side / past the screen edge, Esc or a lost capture,
//! the drag is cancelled (nothing is removed by accident). Ctrl+Z with the
//! pointer on the dock puts the last removed pin back for UNDO_MS.

use super::*;
use std::cell::Cell;
use std::time::Duration;
use windows::Foundation::TimeSpan;
use windows::Win32::Foundation::POINTL;
use windows::Win32::System::Com::{IDataObject, DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL};
use windows::Win32::System::Ole::{
    IDropTarget, IDropTarget_Impl, ReleaseStgMedium, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE,
    DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, RegisterHotKey, UnregisterHotKey, MOD_NOREPEAT, VK_ESCAPE};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Shell::{CLSID_DragDropHelper, DragQueryFileW, IDropTargetHelper, HDROP};
use windows_core::implement;

/// An icon being dragged on the dock.
pub(super) struct Drag {
    path: String,
    was_pinned: bool,
    /// The pinned order before the drag, to put back on cancel.
    original: Vec<String>,
    /// Pointer (canonical u, v).
    cur: (f32, f32),
    /// Where it started (an unpinned app dragged back out of the pinned
    /// area returns there).
    start: usize,
    /// The pointer is off the dock (remove or cancel zone): the others
    /// keep their places and stop magnifying.
    away: bool,
    /// A pinned icon pulled off the dock (in the remove zone).
    off: bool,
    /// ... long enough that "Remove" shows: releasing now removes the pin.
    armed: bool,
    /// Where it was last drawn: center along, far edge across (canonical).
    drawn: Option<(f32, f32)>,
    /// Held over another running app's icon: Split View (split.rs).
    pub(super) split: super::split::Arm,
}

/// Pointer this far beyond the dock's ends or past the screen edge (DIP)
/// cancels the drag.
const CANCEL_DIST: f32 = 60.0;
/// A pinned icon pulled this far off the bar (DIP, across the dock, away
/// from the screen edge) is in the remove zone.
pub(super) const REMOVE_DIST: f32 = 64.0;
/// Time in the remove zone before "Remove" shows (waits about as
/// long); a release before that puts the icon back.
const REMOVE_ARM_MS: u32 = 500;
/// One-shot: the remove wait is over.
pub(super) const REMOVE_TIMER: usize = 20;
/// Removal: fade out in place.
const POOF_MS: i64 = 200;


/// The poof's length at the chosen animation speed (crate::effects).
fn poof_ms() -> i64 {
    crate::effects::ms(POOF_MS)
}
/// One-shot: poofed visuals can go.
pub(super) const POOF_TIMER: usize = 21;
/// How long Ctrl+Z on the dock brings the last removed pin back.
pub(super) const UNDO_MS: u32 = 10_000;
/// One-shot: the undo time is over.
pub(super) const UNDO_TIMER: usize = 22;
/// Dragged icon opacity: off the dock, and armed for removal.
const OFF_ALPHA: f32 = 0.8;
const ARMED_ALPHA: f32 = 0.45;

/// Where a dragged icon is, for what a release would do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Zone {
    /// On (or near) the dock: reorder / pin.
    Dock,
    /// Pulled off the dock, away from the screen edge: remove (pins only).
    Remove,
    /// Off the dock's ends or past the screen edge: put it back.
    Cancel,
}

/// The zone of the pointer (`u` along, `v` across, canonical: the screen
/// edge at `depth`, the desktop side towards 0) for the bar `bar`. Pure;
/// the same for every dock position (edge.rs maps them).
pub(super) fn zone(u: f32, v: f32, bar: Rect, depth: f32) -> Zone {
    // Away from the dock across it wins, even past its ends (the remove zone wins).
    if bar.y - v >= REMOVE_DIST {
        return Zone::Remove;
    }
    let far = v > depth + CANCEL_DIST || u < bar.x - CANCEL_DIST || u > bar.x + bar.w + CANCEL_DIST;
    if far { Zone::Cancel } else { Zone::Dock }
}

/// Opacity of the dragged icon.
pub(super) fn drag_alpha(off: bool, armed: bool) -> f32 {
    if armed {
        ARMED_ALPHA
    } else if off {
        OFF_ALPHA
    } else {
        1.0
    }
}

/// The last pin removed by dragging it off (Ctrl+Z puts it back).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Undo {
    pub entry: PinEntry,
    /// Its place among the pins.
    pub index: usize,
    /// Dividers that went with it (left redundant), each with its place
    /// among the pins before: they come back with it.
    pub also: Vec<(PinEntry, usize)>,
    pub at: Instant,
}

/// Can the removal made at `at` still be undone at `now`?
pub(super) fn undo_fresh(at: Instant, now: Instant) -> bool {
    now.saturating_duration_since(at) < Duration::from_millis(UNDO_MS as u64)
}

/// `pins` with `entry` back at `index` (the end if the list got shorter);
/// None if it is pinned again already.
pub(super) fn reinsert_pin(pins: &[PinEntry], entry: &PinEntry, index: usize) -> Option<Vec<PinEntry>> {
    if pins.iter().any(|p| p.path.eq_ignore_ascii_case(&entry.path)) {
        return None;
    }
    let mut out = pins.to_vec();
    out.insert(index.min(out.len()), entry.clone());
    Some(out)
}

/// `pins` with `entry` back at `index` and the dividers that went with it
/// (`also`) back at theirs, by their places before (each at the end if the
/// list got shorter); in place order: what is pinned again already is
/// skipped. None if `entry` is pinned again already.
pub(super) fn reinsert_pins(pins: &[PinEntry], entry: &PinEntry, index: usize, also: &[(PinEntry, usize)]) -> Option<Vec<PinEntry>> {
    let mut out = reinsert_pin(pins, entry, index)?;
    let mut all: Vec<(PinEntry, usize)> = also.to_vec();
    all.push((entry.clone(), index));
    all.sort_by_key(|(_, i)| *i);
    // Put back again in place order (`entry` among them), so each index
    // means what it meant before the removal.
    out.retain(|p| !p.path.eq_ignore_ascii_case(&entry.path));
    for (e, i) in &all {
        if let Some(next) = reinsert_pin(&out, e, *i) {
            out = next;
        }
    }
    Some(out)
}

/// `new` pins put into `latest` before the pin `before` (by path), or at
/// the end if it is gone (or None: dropped after the last pin). Pins that
/// are there already are skipped.
pub(super) fn insert_pins_before(latest: &[PinEntry], new: &[PinEntry], before: Option<&str>) -> Vec<PinEntry> {
    let mut out = latest.to_vec();
    let mut at = before.and_then(|b| out.iter().position(|p| p.path.eq_ignore_ascii_case(b))).unwrap_or(out.len());
    for n in new {
        if out.iter().any(|p| p.path.eq_ignore_ascii_case(&n.path)) {
            continue;
        }
        out.insert(at, n.clone());
        at += 1;
    }
    out
}

/// Index (among the other items) the dragged one should take: the number
/// of other items whose centers are before the pointer. Pure.
pub(super) fn insert_index(centers: &[f32], u: f32) -> usize {
    centers.iter().filter(|&&c| c < u).count()
}

/// Dropping files on an app icon opens them with it, unless they can be
/// pinned and should be: shortcuts / programs always pin (between the icons
/// a gap opens), anything else pins only off the middle of an icon. Pure.
pub(super) fn opens_with(fresh_pins: bool, all_programs: bool, on_icon_middle: bool) -> bool {
    !fresh_pins || (!all_programs && on_icon_middle)
}

/// The pointer `u` is over the middle half of an icon drawn at `dx`, `w` wide.
pub(super) fn on_middle(u: f32, dx: f32, w: f32) -> bool {
    (u - (dx + w / 2.0)).abs() < w / 4.0
}

/// RegisterHotKey id of Esc, held only while an icon is dragged (the dock
/// never has the keyboard focus).
pub(super) const ESC_HOTKEY: i32 = 0x5557;
/// Repeating, only while dragging: Esc is also polled (the hotkey may be
/// taken by another app), and a hotkey that came while the dock was busy is
/// applied.
pub(super) const ESC_POLL_TIMER: usize = 23;
const ESC_POLL_MS: u32 = 30;
/// Esc was pressed during the current drag (sticky until the next drag
/// starts, so a press that is already released still cancels).
static ESC_HIT: AtomicBool = AtomicBool::new(false);

/// Esc cancels the drag: pressed during it (hotkey / poll), or held now.
pub(super) fn escape_down() -> bool {
    ESC_HIT.load(Ordering::SeqCst) || unsafe { GetAsyncKeyState(VK_ESCAPE.0 as i32) as u16 & 0x8000 != 0 }
}

/// The Esc hotkey fired (WM_HOTKEY): remembered at once, applied when the
/// dock is free.
pub(super) fn escape_hit() {
    ESC_HIT.store(true, Ordering::SeqCst);
}

/// What dropping files would do.
#[derive(Clone, Debug, PartialEq)]
enum DropAction {
    Recycle,
    OpenWith(String),
    /// Into this stack's folder (move / copy / link as Explorer would).
    Stack(String),
    /// Pin them at this index among the pins.
    Pin(usize),
    Nothing,
}

impl DropAction {
    fn effect(&self) -> DROPEFFECT {
        match self {
            DropAction::Recycle => DROPEFFECT_MOVE,
            DropAction::OpenWith(_) => DROPEFFECT_COPY,
            // (Decided by stacks::drop_into, never narrowed here.)
            DropAction::Stack(_) => DROPEFFECT_MOVE,
            DropAction::Pin(_) => DROPEFFECT_LINK,
            DropAction::Nothing => DROPEFFECT_NONE,
        }
    }
}

impl Dock {
    pub(super) fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// The dragged item for layout: path, pointer (u, v), off the dock,
    /// armed for removal.
    pub(super) fn drag_view(&self) -> Option<(&str, f32, f32, bool, bool)> {
        self.drag.as_ref().map(|d| (d.path.as_str(), d.cur.0, d.cur.1, d.off, d.armed))
    }

    /// A dragged icon is off the dock (no magnification then).
    pub(super) fn drag_away(&self) -> bool {
        self.drag.as_ref().is_some_and(|d| d.away)
    }

    /// Layout drew the dragged icon here (center along, far edge across).
    pub(super) fn note_drag_drawn(&mut self, at: Option<(f32, f32)>) {
        if let Some(d) = self.drag.as_mut() {
            d.drawn = at;
        }
    }

    /// Where the "Remove" label goes (icon center along, its far edge), if
    /// it shows.
    pub(super) fn remove_hint_at(&self) -> Option<(f32, f32)> {
        self.drag.as_ref().filter(|d| d.armed).and_then(|d| d.drawn)
    }

    /// The pressed icon moved far enough: start dragging it.
    pub(super) fn maybe_start_drag(&mut self, u: f32, v: f32) {
        let (Some((i, b)), Some((pu, pv))) = (self.pressed, self.press_at) else { return };
        if b != WM_LBUTTONUP || self.drag.is_some() || !self.items.get(i).is_some_and(|it| it.movable()) {
            return;
        }
        // The list may have changed since the press: same app, or nothing.
        if self.press_path.as_deref() != Some(self.items[i].path.as_str()) {
            self.pressed = None;
            self.press_at = None;
            return;
        }
        let th = unsafe { GetSystemMetrics(SM_CXDRAG) }.max(4) as f32 / self.scale;
        if (u - pu).abs() < th && (v - pv).abs() < th {
            return;
        }
        let it = &self.items[i];
        let _ = it.icon.SetOpacity(1.0);
        self.drag = Some(Drag {
            path: it.path.clone(),
            was_pinned: it.pinned,
            original: self.items.iter().filter(|x| x.pin_slot()).map(|x| x.path.clone()).collect(),
            cur: (u, v),
            start: i,
            away: false,
            off: false,
            armed: false,
            drawn: None,
            split: Default::default(),
        });
        preview::hide();
        // No previews while dragging.
        self.hover_item = None;
        self.hover_app = None;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), PREVIEW_TIMER);
        }
        unsafe {
            SetCapture(self.hwnd);
        }
        // Esc cancels: held as a hotkey for the drag's lifetime, and polled
        // too (another app may have it). Ctrl+Z is let go meanwhile.
        ESC_HIT.store(false, Ordering::SeqCst);
        let held = unsafe { RegisterHotKey(Some(self.hwnd), ESC_HOTKEY, MOD_NOREPEAT, VK_ESCAPE.0 as u32).is_ok() };
        if !held {
            crate::log!("drag: Esc is taken by another app; only polled while dragging");
        }
        unsafe {
            SetTimer(Some(self.hwnd), ESC_POLL_TIMER, ESC_POLL_MS, None);
        }
    }

    /// ESC_POLL_TIMER fired / the Esc hotkey came: cancel if Esc was
    /// pressed; the timer stops with the drag.
    pub(super) fn escape_check(&mut self) {
        if !self.dragging() {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), ESC_POLL_TIMER);
            }
            return;
        }
        if escape_down() {
            crate::log!("drag: cancelled with Esc");
            self.end_drag(true, None);
            self.pressed = None;
            return;
        }
        // The split picture follows Shift (2/3 + 1/3).
        self.split_poll();
    }

    /// Pointer moved while dragging: move the item to where it would land,
    /// or mark it for removal while it is off the dock.
    pub(super) fn drag_move(&mut self, u: f32, v: f32) {
        if escape_down() {
            self.end_drag(true, None);
            self.pressed = None;
            return;
        }
        let (bar, depth, hwnd) = (self.bar_rect, self.frame.depth, self.hwnd);
        let z = zone(u, v, bar, depth);
        // Over the middle of another running app's icon: Split View arms
        // (after a moment) and the order holds still meanwhile.
        let over = if z == Zone::Dock { self.split_candidate(u, v) } else { None };
        let splitting = self.split_hover(over);
        let Some(d) = self.drag.as_mut() else { return };
        d.cur = (u, v);
        d.away = z != Zone::Dock;
        // Only pins can be removed; a running app just goes back.
        let off = z == Zone::Remove && d.was_pinned;
        if off != d.off {
            d.off = off;
            d.armed = false;
            unsafe {
                if off {
                    SetTimer(Some(hwnd), REMOVE_TIMER, REMOVE_ARM_MS, None);
                } else {
                    let _ = KillTimer(Some(hwnd), REMOVE_TIMER);
                }
            }
        }
        if z != Zone::Dock || splitting {
            // Off the dock (or over a split target): the others keep
            // their places.
            self.kick();
            return;
        }
        let (path, start) = (d.path.clone(), d.start);
        let Some(from) = self.items.iter().position(|it| it.movable() && it.path == path) else { return };
        let item = self.items.remove(from);
        // Centers of the others as laid out now.
        let centers: Vec<f32> = self.items.iter().map(|it| it.x + it.w / 2.0).collect();
        let first = self.items.iter().take_while(|it| it.kind == Kind::Start).count();
        let sep = self.sep_index();
        // Pinned area only: before the separator (a running app dragged
        // there gets pinned on release); elsewhere it stays where it was.
        let mut to = insert_index(&centers, u).max(first);
        if to > sep {
            // Outside the pinned area: pinned apps stay at its end, an
            // unpinned one goes back to where it came from.
            to = if item.pinned { sep } else { start.min(self.items.len()) };
        }
        self.items.insert(to.min(self.items.len()), item);
        self.kick();
    }

    /// REMOVE_TIMER fired: the icon has been off the dock long enough.
    pub(super) fn remove_wait_over(&mut self) {
        if escape_down() {
            if self.end_drag(true, None) {
                self.pressed = None;
            }
            return;
        }
        let Some(d) = self.drag.as_mut() else { return };
        if d.off && !d.armed {
            d.armed = true;
            self.layout();
            self.kick();
        }
    }

    /// Button released while dragging. Returns true if a drag ended (the
    /// release is then not a click).
    pub(super) fn end_drag(&mut self, cancel: bool, at: Option<(f32, f32)>) -> bool {
        // Split View only if the release point itself is over the armed
        // target's middle (the last move may be older); its side counts.
        let release = at.and_then(|(u, v)| self.split_candidate(u, v)).map(|(t, s)| (self.items[t].path.clone(), s));
        let guided = self.split_shown();
        let Some(mut d) = self.drag.take() else { return false };
        if let Some(p) = at {
            d.cur = p;
        }
        unsafe {
            let _ = KillTimer(Some(self.hwnd), REMOVE_TIMER);
            let _ = KillTimer(Some(self.hwnd), ESC_POLL_TIMER);
            let _ = UnregisterHotKey(Some(self.hwnd), ESC_HOTKEY);
            let _ = ReleaseCapture();
        }
        let (u, v) = d.cur;
        let z = zone(u, v, self.bar_rect, self.frame.depth);
        let cancel = cancel || escape_down();
        // Released over an armed split target: Split View, not a reorder.
        let armed = d.split.armed().map(|(t, _)| t.to_string());
        let split = release.filter(|(t, _)| !cancel && z == Zone::Dock && armed.as_deref() == Some(t.as_str()));
        self.split_end();
        if let Some((target, side)) = split {
            self.restore_order(&d);
            // Shift at the release: the dragged app gets two thirds.
            let ratio = super::split::ratio_now();
            self.do_split(&d.path, &target, side, ratio, guided.as_ref());
        } else if !cancel && d.was_pinned && d.armed && z == Zone::Remove {
            self.remove_dragged(&d);
        } else if cancel || z != Zone::Dock {
            self.restore_order(&d);
        } else if let Some(i) = self.find_dragged(&d.path) {
            if i < self.sep_index() {
                self.items[i].pinned = true;
            }
            let order: Vec<String> = self.items.iter().filter(|x| x.pin_slot()).map(|x| x.path.clone()).collect();
            if order != d.original {
                // Dividers left redundant by the move go (not while
                // dragging: only once it is dropped).
                let gone = self.drop_redundant_dividers();
                let now: Vec<PinEntry> = self.items.iter().filter(|x| x.pin_slot()).map(|x| self.pin_entry(x)).collect();
                // The items already show this order: no rebuild unless the
                // file changed meanwhile.
                self.cfg.dock.pins = now.clone();
                // Merge into the newest pins on disk: keep pins added or
                // removed elsewhere meanwhile, apply only our order.
                // (Dividers it left redundant go: dividers::reordered.)
                let dragged = d.path.clone();
                self.update_config(move |c| c.dock.pins = super::dividers::reordered(&c.dock.pins, &now, &dragged, &gone));
                self.sync_windows();
            }
        }
        self.kick();
        // Released off the dock: the leave was held back while dragging.
        if !self.cursor_over_window() {
            self.on_mouse_leave();
        }
        true
    }

    /// Cancelled: the pinned order as it was, the item pinned as it was.
    fn restore_order(&mut self, d: &Drag) {
        self.items.sort_by_key(|it| match it.kind {
            Kind::Start => (0, 0),
            Kind::App | Kind::Divider if it.pinned => (1, d.original.iter().position(|p| *p == it.path).unwrap_or(usize::MAX)),
            Kind::Sep => (2, 0),
            Kind::App | Kind::Divider => (3, 0),
            Kind::Trash => (4, 0),
        });
        if let Some(i) = self.items.iter().position(|it| it.path == d.path && it.movable()) {
            self.items[i].pinned = d.was_pinned;
        }
    }

    /// Released off the dock: the pin goes. A running app stays (with the
    /// running ones); anything else goes "poof" and the others close up.
    fn remove_dragged(&mut self, d: &Drag) {
        self.restore_order(d);
        let Some(i) = self.find_dragged(&d.path) else { return };
        let index = d.original.iter().position(|p| *p == d.path).unwrap_or(d.original.len());
        let entry = self.pin_entry(&self.items[i]);
        let running = !self.items[i].windows.is_empty();
        if running {
            let mut it = self.items.remove(i);
            it.pinned = false;
            self.items.push(it);
        } else {
            let it = self.items.remove(i);
            self.poof(it);
        }
        // Dividers left redundant by it go with it (and come back with
        // it on Ctrl+Z).
        let dropped = self.drop_redundant_dividers();
        let also = super::dividers::went_along(&d.original, &dropped);
        let mut gone = dropped;
        gone.push(d.path.clone());
        // The items already show the new list: no rebuild.
        self.cfg.dock.pins = super::dividers::without(&self.cfg.dock.pins, &gone);
        self.update_config(move |c| c.dock.pins = super::dividers::without(&c.dock.pins, &gone));
        self.sync_windows();
        crate::log!(
            "dock: pin {:?} removed by dragging it off (was #{index}{}); Ctrl+Z on the dock for {} s puts it back",
            entry.path,
            if running { ", still running" } else { "" },
            UNDO_MS / 1000
        );
        self.remember_removal(entry, index, also);
    }

    /// Ctrl+Z on the dock puts this removal back (for UNDO_MS): `entry`
    /// at `index` among the pins, with the dividers that went along
    /// (`also`). Replaces an older removal's undo.
    pub(super) fn remember_removal(&mut self, entry: PinEntry, index: usize, also: Vec<(PinEntry, usize)>) {
        self.undo = Some(Undo { entry, index, also, at: Instant::now() });
        unsafe {
            SetTimer(Some(self.hwnd), UNDO_TIMER, UNDO_MS, None);
        }
    }

    /// The removed item's icon fades out where it was
    /// dropped (at once when animations are off).
    pub(super) fn poof(&mut self, mut it: Item) {
        let Ok(ch) = self.layer.Children() else { return };
        let _ = ch.Remove(&it.dot);
        let _ = ch.Remove(&it.tile);
        it.marks.remove_from(&ch);
        if !motion() || self.start_poof(&it).is_err() {
            let _ = ch.Remove(&it.icon);
            let _ = ch.Remove(&it.plate);
            return;
        }
        self.poofs.push((Instant::now(), it.icon, it.plate));
        unsafe {
            SetTimer(Some(self.hwnd), POOF_TIMER, poof_ms() as u32 + 40, None);
        }
    }

    fn start_poof(&self, it: &Item) -> Result<()> {
        let c = &self.gfx.compositor;
        // (Its own motion, like the bounce: its length stays; the curve
        // is the kit's ease-out.)
        let ((x1, y1), (x2, y2)) = crate::motion::EASE_OUT;
        let ease = c.CreateCubicBezierEasingFunction(Vector2 { X: x1, Y: y1 }, Vector2 { X: x2, Y: y2 })?;
        let dur = TimeSpan { Duration: poof_ms() * 10_000 };
        for v in [&it.icon, &it.plate] {
            let size = v.Size()?;
            v.SetCenterPoint(Vector3 { X: size.X / 2.0, Y: size.Y / 2.0, Z: 0.0 })?;
            let fade = c.CreateScalarKeyFrameAnimation()?;
            fade.InsertKeyFrameWithEasingFunction(1.0, 0.0, &ease)?;
            fade.SetDuration(dur)?;
            v.StartAnimation(&HSTRING::from("Opacity"), &fade)?;
        }
        Ok(())
    }

    /// POOF_TIMER fired: drop the visuals whose poof is over.
    pub(super) fn poof_done(&mut self) {
        let Ok(ch) = self.layer.Children() else { return };
        let now = Instant::now();
        self.poofs.retain(|(at, icon, plate)| {
            let done = now.duration_since(*at).as_millis() as i64 >= poof_ms();
            if done {
                let _ = ch.Remove(icon);
                let _ = ch.Remove(plate);
            }
            !done
        });
        if !self.poofs.is_empty() {
            unsafe {
                SetTimer(Some(self.hwnd), POOF_TIMER, 50, None);
            }
        }
    }

    // ---- undo ---------------------------------------------------------------

    pub(super) fn can_undo(&self) -> bool {
        self.undo.as_ref().is_some_and(|u| undo_fresh(u.at, Instant::now()))
    }

    /// UNDO_TIMER fired: the removal can no longer be undone.
    pub(super) fn undo_expired(&mut self) {
        if self.undo.as_ref().is_some_and(|u| !undo_fresh(u.at, Instant::now())) {
            self.undo = None;
        }
    }

    /// Ctrl+Z on the dock: the last removed pin goes back where it was.
    pub(super) fn undo_remove(&mut self) {
        // Re-checked: only with the pointer really on the dock, not dragging.
        if self.dragging() || self.ole_drag {
                return;
        }
        let u = self.undo.take();
        unsafe {
            let _ = KillTimer(Some(self.hwnd), UNDO_TIMER);
        }
        let Some(u) = u else { return };
        if !undo_fresh(u.at, Instant::now()) {
            crate::log!("dock: undo of the pin removal came too late; nothing changed");
            return;
        }
        if self.cfg.dock.pins.iter().any(|p| p.path.eq_ignore_ascii_case(&u.entry.path)) {
            crate::log!("dock: undo: {:?} is pinned again already", u.entry.path);
            return;
        }
        // With the dividers that went with it, in place order.
        let mut back: Vec<(PinEntry, usize)> = u.also.clone();
        back.push((u.entry.clone(), u.index));
        back.sort_by_key(|(_, i)| *i);
        for (e, i) in &back {
            if !self.cfg.dock.pins.iter().any(|p| p.path.eq_ignore_ascii_case(&e.path)) {
                self.insert_pin_items(*i, std::slice::from_ref(e), false);
            }
        }
        let (entry, index, also) = (u.entry.clone(), u.index, u.also.clone());
        self.update_config(move |c| {
            if let Some(p) = reinsert_pins(&c.dock.pins, &entry, index, &also) {
                c.dock.pins = p;
            }
        });
        self.sync_windows();
        self.kick();
        crate::log!("dock: undo put pin {:?} back at #{}", u.entry.path, u.index);
    }

    /// Puts pins on the dock at `at` among the pins (a running app with the
    /// same path moves over instead of getting a twin). `fill_gap`: they
    /// take an open drop gap at full size instead of growing in. Also
    /// updates the in-memory pin list, so the save does not rebuild.
    pub(super) fn insert_pin_items(&mut self, at: usize, entries: &[PinEntry], fill_gap: bool) {
        let first = self.items.iter().take_while(|it| it.kind == Kind::Start).count();
        let pinned = self.items.iter().filter(|it| it.pin_slot()).count();
        let mut pos = first + at.min(pinned);
        let mut unknown = Vec::new();
        for e in entries {
            if e.is_divider() {
                let Ok(it) = self.new_item(Kind::Divider, String::new(), e.path.clone(), true, AppId::default()) else { continue };
                self.items.insert(pos.min(self.items.len()), it);
                pos += 1;
                continue;
            }
            let reuse = self.items.iter().position(|it| it.kind == Kind::App && !it.pinned && it.path.eq_ignore_ascii_case(&e.path));
            let mut it = match reuse {
                Some(i) => {
                    if i < pos {
                        pos -= 1;
                    }
                    self.items.remove(i)
                }
                None => {
                    let kind = crate::stacks::known_kind(&e.path);
                    let stack = kind == Some(true);
                    if kind.is_none() && crate::stacks::could_be_folder(&e.path) {
                        unknown.push(e.path.clone());
                    }
                    if let Some(w) = &e.web {
                        crate::webapps::seed(w);
                    }
                    let id = if stack { AppId::default() } else { apps::identify_pin(&e.path, e.web.as_ref()) };
                    let Ok(mut it) = self.new_item(Kind::App, e.name.clone(), e.path.clone(), true, id) else { continue };
                    it.stack = stack;
                    if fill_gap {
                        it.pres = 1.0;
                    }
                    it
                }
            };
            it.pinned = true;
            it.pres_t = 1.0;
            self.items.insert(pos.min(self.items.len()), it);
            pos += 1;
        }
        if !unknown.is_empty() {
            self.workers.check_pin_kinds(unknown);
        }
        self.cfg.dock.pins = self.items.iter().filter(|x| x.pin_slot()).map(|x| self.pin_entry(x)).collect();
    }

    // ---- files dropped from Explorer ------------------------------------------

    fn screen_to_canon(&self, pt: POINTL) -> (f32, f32) {
        let x = (pt.x - self.origin.x) as f32 / self.scale;
        let y = (pt.y - self.origin.y) as f32 / self.scale;
        self.frame.from_win(x, y)
    }

    fn is_pinned(&self, path: &str) -> bool {
        let p = path.trim_end_matches('\\');
        self.cfg.dock.pins.iter().any(|x| x.path.eq_ignore_ascii_case(p))
    }

    /// Index among the pins a drop at `u` takes (same math as reordering).
    fn pin_drop_index(&self, u: f32) -> usize {
        let centers: Vec<f32> = self.items.iter().filter(|it| it.pin_slot()).map(|it| it.x + it.w / 2.0).collect();
        insert_index(&centers, u)
    }

    /// What dropping `files` at `pt` would do, and its effect (already
    /// narrowed to what the source allows; Nothing when none is). `pins`:
    /// the ones that can be pinned (shortcuts, programs, folders); `keys`:
    /// the drag's key state.
    fn drop_action(&self, files: &[String], pins: &[String], keys: u32, source: DROPEFFECT, pt: POINTL) -> (DropAction, DROPEFFECT) {
        if files.is_empty() {
            return (DropAction::Nothing, DROPEFFECT_NONE);
        }
        let (u, _) = self.screen_to_canon(pt);
        let fresh = pins.iter().any(|f| !self.is_pinned(f));
        let programs = files.iter().all(|f| pinnable(f));
        let a = match self.hit(u).map(|i| &self.items[i]) {
            // Into the stack's folder, like dropping on a folder in Explorer.
            Some(it) if it.kind == Kind::App && it.stack => {
                let e = DROPEFFECT(crate::stacks::drop_into(keys, files, &it.path, source.0));
                return if e == DROPEFFECT_NONE { (DropAction::Nothing, e) } else { (DropAction::Stack(it.path.clone()), e) };
            }
            Some(it) if it.kind == Kind::Trash => DropAction::Recycle,
            Some(it) if it.kind == Kind::App && !it.path.is_empty() && opens_with(fresh, programs, on_middle(u, it.dx, it.w)) => {
                DropAction::OpenWith(it.path.clone())
            }
            _ if fresh => DropAction::Pin(self.pin_drop_index(u)),
            _ => DropAction::Nothing,
        };
        let e = allowed(a.effect(), source);
        if e == DROPEFFECT_NONE { (DropAction::Nothing, e) } else { (a, e) }
    }

    /// Opens a gap among the pins where a drop would go (None closes it).
    fn set_drop_gap(&mut self, at: Option<usize>) {
        let first = self.items.iter().take_while(|it| it.kind == Kind::Start).count();
        let pinned = self.items.iter().filter(|it| it.pin_slot()).count();
        let target = at.map(|a| first + a.min(pinned));
        let mut changed = false;
        for (i, it) in self.items.iter_mut().enumerate() {
            let t = if Some(i) == target { 1.0 } else { 0.0 };
            if it.gap_t != t {
                it.gap_t = t;
                changed = true;
            }
            if !glides() {
                it.gap = t;
            }
        }
        if changed {
            self.kick();
        }
    }

    /// The stack that dragged files would go into (None: none): lights it.
    pub(super) fn set_drop_stack(&mut self, path: Option<String>) {
        if self.drop_stack == path {
            return;
        }
        self.drop_stack = path;
        if self.drop_stack.is_some() {
            // The selected row's wash (no hue: a drop target is not "on";
            // design B3), or Highlight in high contrast (opaque there).
            let c = match self.theme.focus_outline() {
                Some(h) => color(255, h.r, h.g, h.b),
                None => rgba(self.theme.fill_row(crate::theme::State::Selected)),
            };
            if let Ok(b) = self.gfx.compositor.CreateColorBrushWithColor(c) {
                let _ = self.drop_glow.SetBrush(&b);
            }
        }
        self.place_drop_glow();
    }

    /// Puts the glow behind the lit stack's icon (after every layout).
    pub(super) fn place_drop_glow(&mut self) {
        let it = self.drop_stack.as_ref().and_then(|p| self.items.iter().find(|it| it.kind == Kind::App && it.stack && it.path == *p));
        let Some((Ok(o), Ok(sz))) = it.map(|it| (it.plate.Offset(), it.plate.Size())) else {
            if std::mem::take(&mut self.glow_on) {
                show(&self.gfx.compositor, &self.drop_glow, false);
            }
            return;
        };
        let m = crate::theme::metrics::space::S2;
        let size = Vector2 { X: sz.X + m * 2.0, Y: sz.Y + m * 2.0 };
        let _ = self.drop_glow.SetOffset(Vector3 { X: o.X - m, Y: o.Y - m, Z: 0.0 });
        let _ = self.drop_glow.SetSize(size);
        let _ = self.drop_glow_geom.SetSize(size);
        // Concentric with the icon's squircle, the glow's margin out.
        let r = (size.X.min(size.Y) - m * 2.0) * crate::theme::SQUIRCLE_ARC + m;
        let _ = self.drop_glow_geom.SetCornerRadius(Vector2 { X: r, Y: r });
        if !std::mem::replace(&mut self.glow_on, true) {
            show(&self.gfx.compositor, &self.drop_glow, true);
        }
    }

    /// Whether `pt` is over a stack.
    fn drop_stack_hit(&self, pt: POINTL) -> bool {
        let (u, _) = self.screen_to_canon(pt);
        self.hit(u).is_some_and(|i| self.items[i].kind == Kind::App && self.items[i].stack)
    }

    /// A stack's folder changed (files moved out or in). Stacks keep no
    /// listing between opens, so only a listing still on its way is stale:
    /// ask again so the panel shows the folder as it is now.
    pub(super) fn stack_refresh(&mut self, folder: &str) {
        let pending = self.stack_req.as_ref().is_some_and(|(_, p, _)| p.eq_ignore_ascii_case(folder));
        if pending {
            if let Some(i) = self.items.iter().position(|it| it.kind == Kind::App && it.stack && it.path.eq_ignore_ascii_case(folder)) {
                self.open_stack(i);
            }
        }
    }

    /// A transfer into a stack finished: a short hop says it landed.
    pub(super) fn stack_dropped(&mut self, folder: &str, ok: bool) {
        self.stack_refresh(folder);
        if !ok {
            return;
        }
        let hop = motion() && crate::effects::items().bounce;
        if let Some(it) = self.items.iter_mut().find(|it| it.kind == Kind::App && it.stack && it.path.eq_ignore_ascii_case(folder)) {
            if hop {
                it.bounce = Some(Instant::now());
                self.kick();
            }
        }
    }

    /// Does the drop. Returns false if the source must not act on its own
    /// side (e.g. the files were already sent to the Recycle Bin).
    fn drop_files(&mut self, action: DropAction, files: Vec<String>, pins: Vec<String>, effect: DROPEFFECT) -> bool {
        match action {
            // Into a stack's folder, on a thread of its own (Windows' copy
            // engine shows progress, conflicts, elevation).
            DropAction::Stack(folder) => {
                self.workers.drop_into_stack(files, folder, effect.0);
                // We move them ourselves: the source must not delete.
                return effect != DROPEFFECT_MOVE;
            }
            // Into the Recycle Bin (undoable, Windows shows its own UI).
            DropAction::Recycle => {
                unsafe { recycle(self.hwnd, &files) };
                self.refresh_trash();
                // We did the work ourselves: the source must not delete.
                return false;
            }
            // Open them with that app.
            DropAction::OpenWith(app) => {
                let args: Vec<String> = files.iter().map(|f| format!("\"{f}\"")).collect();
                let (a, p) = (HSTRING::from(app.as_str()), HSTRING::from(args.join(" ")));
                crate::launch::open_soon(crate::launch::Target::new(&a.to_string(), &app, crate::launch::Source::Dock).args(&p.to_string(), ""));
                if let Some(i) = self.find_app_index(&app) {
                    if self.items[i].windows.is_empty() && motion() && crate::effects::items().bounce {
                        self.items[i].bounce = Some(Instant::now());
                        self.kick();
                    }
                }
            }
            // Pin shortcuts, programs and folders (folders become stacks)
            // where they were dropped.
            DropAction::Pin(at) => {
                let new: Vec<PinEntry> = pins
                    .into_iter()
                    .filter(|f| !self.is_pinned(f))
                    .map(|f| {
                        // Folder-ness was checked off the UI thread (shell
                        // attributes); names come from that check too.
                        if crate::stacks::known_kind(&f) == Some(true) {
                            let name = crate::stacks::known_name(&f).unwrap_or_else(|| crate::stacks::last_part(&f));
                            let path = if f.len() > 3 { f.trim_end_matches('\\').to_string() } else { f };
                            PinEntry::new(name, path)
                        } else {
                            PinEntry::new(std::path::Path::new(&f).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(), f)
                        }
                    })
                    .collect();
                if !new.is_empty() {
                    // The pin the new ones go before (by path: the file may
                    // differ from what the dock shows).
                    let before = self.items.iter().filter(|it| it.pin_slot()).nth(at).map(|it| it.path.clone());
                    // The gap becomes the new icon, without a jump.
                    for it in &mut self.items {
                        it.gap = 0.0;
                        it.gap_t = 0.0;
                    }
                    self.insert_pin_items(at, &new, true);
                    crate::log!("drop: pinned {} item(s) at #{at}", new.len());
                    self.update_config(move |c| c.dock.pins = insert_pins_before(&c.dock.pins, &new, before.as_deref()));
                    self.sync_windows();
                    self.kick();
                }
            }
            DropAction::Nothing => {}
        }
        true
    }

    fn find_app_index(&self, path: &str) -> Option<usize> {
        self.items.iter().position(|it| it.kind == Kind::App && it.path == path)
    }

    /// The dragged item (an app or a divider), by its path.
    fn find_dragged(&self, path: &str) -> Option<usize> {
        self.items.iter().position(|it| it.movable() && it.path == path)
    }
}

/// A full, plain file path we are willing to delete: "X:\..." or a UNC
/// share path, no wildcards, not a drive root, no device namespace.
pub(super) fn safe_to_recycle(p: &str) -> bool {
    let b = p.as_bytes();
    let drive = b.len() > 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\';
    let unc = p.starts_with("\\\\") && !p.starts_with("\\\\?\\") && !p.starts_with("\\\\.\\") && p[2..].matches('\\').count() >= 2;
    (drive || unc) && !p.contains(['*', '?']) && !p.ends_with('\\')
}

/// Moves files to the Recycle Bin (SHFileOperation with undo, Windows'
/// own confirmation and warning before anything is deleted for good).
unsafe fn recycle(owner: HWND, files: &[String]) {
    use windows::Win32::UI::Shell::{SHFileOperationW, FOF_ALLOWUNDO, FOF_WANTNUKEWARNING, FO_DELETE, SHFILEOPSTRUCTW};
    if files.is_empty() {
        return;
    }
    if let Some(bad) = files.iter().find(|f| !safe_to_recycle(f)) {
        crate::log!("trash: refused odd path {bad:?}; nothing deleted");
        return;
    }
    // Double-NUL-terminated list.
    let mut from: Vec<u16> = Vec::new();
    for f in files {
        from.extend(f.encode_utf16());
        from.push(0);
    }
    from.push(0);
    let mut op = SHFILEOPSTRUCTW {
        hwnd: owner,
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        fFlags: (FOF_ALLOWUNDO.0 | FOF_WANTNUKEWARNING.0) as u16,
        ..Default::default()
    };
    let r = SHFileOperationW(&mut op);
    crate::log!("trash: moved {} item(s), result {r}", files.len());
}

/// Our new pin order applied to `latest` (the file may have changed while
/// dragging): pins we know keep our order, pins only on disk stay (at the
/// end), pins removed on disk stay removed unless it is the one we dragged.
pub(super) fn merge_pins(latest: &[PinEntry], ours: &[PinEntry], dragged: &str) -> Vec<PinEntry> {
    let in_latest = |p: &PinEntry| latest.iter().any(|l| l.path.eq_ignore_ascii_case(&p.path));
    let mut out: Vec<PinEntry> = ours.iter().filter(|p| in_latest(p) || p.path == dragged).cloned().collect();
    for l in latest {
        if !out.iter().any(|o| o.path.eq_ignore_ascii_case(&l.path)) {
            out.push(l.clone());
        }
    }
    out
}

/// Narrow what we would do to what the drag source allows.
pub(super) fn allowed(want: DROPEFFECT, source: DROPEFFECT) -> DROPEFFECT {
    if want.0 & source.0 != 0 {
        return want;
    }
    // Pinning works just as well as a copy.
    if want == DROPEFFECT_LINK && source.0 & DROPEFFECT_COPY.0 != 0 {
        return DROPEFFECT_COPY;
    }
    DROPEFFECT_NONE
}

/// Things that can be pinned by dropping them on the dock.
pub(super) fn pinnable(path: &str) -> bool {
    let l = path.to_lowercase();
    l.ends_with(".lnk") || l.ends_with(".exe") || l.ends_with(".url")
}

/// What in a drag can be pinned: folders confirmed by the attribute check
/// (cache lookups only, no disk access), shortcuts and programs.
pub(super) fn pin_candidates(files: &[String]) -> Vec<String> {
    files.iter().filter(|f| crate::stacks::known_kind(f) == Some(true) || pinnable(f)).cloned().collect()
}

/// "Performed DropEffect": what the target really did (optimized moves).
fn performed_format() -> u16 {
    unsafe { windows::Win32::System::DataExchange::RegisterClipboardFormatW(w!("Performed DropEffect")) as u16 }
}

/// The performed effect a drop target set on `data` (0 if none).
pub(super) unsafe fn performed_effect(data: &IDataObject) -> u32 {
    let fmt = FORMATETC { cfFormat: performed_format(), ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
    let Ok(mut medium) = data.GetData(&fmt) else { return 0 };
    let h = medium.u.hGlobal;
    let p = windows::Win32::System::Memory::GlobalLock(h) as *const u32;
    let v = if p.is_null() { 0 } else { *p };
    if !p.is_null() {
        let _ = windows::Win32::System::Memory::GlobalUnlock(h);
    }
    ReleaseStgMedium(&mut medium);
    v
}

/// Tells the drag source what we did (an optimized move: we moved the
/// files ourselves, the source must not delete them).
pub(super) unsafe fn set_performed_effect(data: &IDataObject, effect: u32) {
    use windows::Win32::System::Com::{STGMEDIUM, STGMEDIUM_0};
    use windows::Win32::Foundation::GlobalFree;
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, 4) else { return };
    let p = GlobalLock(h) as *mut u32;
    if p.is_null() {
        let _ = GlobalFree(Some(h));
        return;
    }
    *p = effect;
    let _ = GlobalUnlock(h);
    let fmt = FORMATETC { cfFormat: performed_format(), ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
    let medium = STGMEDIUM { tymed: TYMED_HGLOBAL.0 as u32, u: STGMEDIUM_0 { hGlobal: h }, pUnkForRelease: std::mem::ManuallyDrop::new(None) };
    // The data object owns the memory once it takes it.
    if data.SetData(&fmt, &medium, true).is_err() {
        let _ = GlobalFree(Some(h));
    }
}

/// File paths in a drag (CF_HDROP).
pub(super) unsafe fn files_of(data: &IDataObject) -> Vec<String> {
    let fmt = FORMATETC { cfFormat: 15, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
    let Ok(mut medium) = data.GetData(&fmt) else { return Vec::new() };
    let hdrop = HDROP(medium.u.hGlobal.0);
    let n = DragQueryFileW(hdrop, u32::MAX, None);
    let mut out = Vec::new();
    if n > 64 {
        // Too many to handle in one go: refuse the whole drop rather than
        // act on part of it.
        ReleaseStgMedium(&mut medium);
        crate::log!("drop: {n} files is too many; ignored");
        return out;
    }
    for i in 0..n {
        let len = DragQueryFileW(hdrop, i, None) as usize;
        let mut buf = vec![0u16; len + 1];
        let got = DragQueryFileW(hdrop, i, Some(&mut buf)) as usize;
        out.push(String::from_utf16_lossy(&buf[..got]));
    }
    ReleaseStgMedium(&mut medium);
    out
}

thread_local! {
    static DROP_FILES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    /// Effects the drag source allows, the drag's key state, and the last
    /// point handled.
    static DROP_ALLOWED: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static DROP_KEYS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    /// The drag is a right-button one (stacks then copy; see choose_effect).
    static DROP_RIGHT: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static DROP_LAST: std::cell::Cell<(i32, i32)> = const { std::cell::Cell::new((i32::MIN, 0)) };
}

/// The shell's drag-image helper: shows the dragged things' picture over
/// our windows like over Explorer's.
pub(super) fn drop_helper() -> Option<IDropTargetHelper> {
    unsafe { CoCreateInstance(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER).ok() }
}

pub(super) const FILE_HOVER_TIMER: usize = 0x4648;
pub(super) const FILE_HOVER_MS: u32 = 950;

impl Dock {
    fn file_hover_at(&self, pt: POINTL) -> Option<usize> {
        let (u, _) = self.screen_to_canon(pt);
        let i = self.hit(u)?;
        let it = &self.items[i];
        (it.kind == Kind::App && !it.stack && !it.windows.is_empty()).then_some(i)
    }

    fn track_file_hover(&mut self, pt: POINTL, files: bool) {
        let next = (files && crate::taskbar::hidden_by_us()).then(|| self.file_hover_at(pt)).flatten()
            .map(|i| self.items[i].path.clone());
        if next == self.file_hover { return; }
        self.file_hover = next;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), FILE_HOVER_TIMER);
            if self.file_hover.is_some() { SetTimer(Some(self.hwnd), FILE_HOVER_TIMER, FILE_HOVER_MS, None); }
        }
    }

    pub(super) unsafe fn file_hover_due(&mut self) {
        if !self.ole_drag || !crate::taskbar::hidden_by_us() { return; }
        let mut pt = POINT::default();
        if GetCursorPos(&mut pt).is_err() || GetAncestor(WindowFromPoint(pt), GA_ROOT) != self.hwnd { return; }
        let Some(i) = self.file_hover_at(POINTL { x: pt.x, y: pt.y }) else { return };
        if self.file_hover.as_ref() != Some(&self.items[i].path) { return; }
        if let [window] = self.items[i].windows.as_slice() {
            input::focus(*window);
        } else {
            self.show_preview(i);
        }
    }
}

/// OLE owns the drag even while its pointer is still. Arm a window timer on
/// entering a card and revoke it on every leave/drop, including cancellation.
#[implement(IDropTarget)]
struct PreviewDrop { hwnd: HWND, files: Cell<bool>, card: Cell<Option<(HWND, u32)>> }

impl PreviewDrop_Impl {
    fn track(&self, pt: &POINTL, effect: *mut DROPEFFECT) {
        if !effect.is_null() { unsafe { *effect = DROPEFFECT_NONE; } }
        let card = if self.files.get() && crate::taskbar::hidden_by_us() {
            preview::file_card(self.hwnd, POINT { x: pt.x, y: pt.y })
        } else { None };
        if self.card.replace(card) == card { return; }
        preview::arm_file_card(self.hwnd, card);
    }
    fn clear(&self) {
        self.files.set(false);
        self.card.set(None);
        preview::arm_file_card(self.hwnd, None);
    }
}

impl IDropTarget_Impl for PreviewDrop_Impl {
    fn DragEnter(&self, data: windows_core::Ref<IDataObject>, _: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> windows_core::Result<()> {
        let fmt = FORMATETC { cfFormat: 15, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
        self.files.set(data.as_ref().is_some_and(|d| unsafe { d.QueryGetData(&fmt).is_ok() }));
        self.track(pt, effect);
        Ok(())
    }
    fn DragOver(&self, _: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> windows_core::Result<()> {
        self.track(pt, effect);
        Ok(())
    }
    fn DragLeave(&self) -> windows_core::Result<()> { self.clear(); Ok(()) }
    fn Drop(&self, _: windows_core::Ref<IDataObject>, _: MODIFIERKEYS_FLAGS, _: &POINTL, effect: *mut DROPEFFECT) -> windows_core::Result<()> {
        self.clear();
        if !effect.is_null() { unsafe { *effect = DROPEFFECT_NONE; } }
        Ok(())
    }
}

pub(super) unsafe fn register_preview(hwnd: HWND) {
    let target: IDropTarget = PreviewDrop { hwnd, files: Cell::new(false), card: Cell::new(None) }.into();
    let _ = windows::Win32::System::Ole::RegisterDragDrop(hwnd, &target);
}

#[implement(IDropTarget)]
pub(super) struct DockDrop {
    hwnd: HWND,
    helper: Option<IDropTargetHelper>,
}

impl DockDrop_Impl {
    /// The effect at `pt` (also lights the stack it would go into).
    fn effect_at(&self, pt: &POINTL, mouse_moved: bool) -> DROPEFFECT {
        let source = DROPEFFECT(DROP_ALLOWED.with(|a| a.get()));
        let keys = DROP_KEYS.with(|k| k.get()) | DROP_RIGHT.with(|r| r.get());
        DROP_FILES.with(|f| {
            let files = f.borrow();
            let pins = pin_candidates(&files);
            with_dock(|d| {
                if mouse_moved {
                    // Magnify under the pointer like a hover (no previews).
                    let (u, v) = d.screen_to_canon(*pt);
                    d.on_mouse_move(u, v);
                }
                d.track_file_hover(*pt, !files.is_empty());
                let (a, e) = d.drop_action(&files, &pins, keys, source, *pt);
                // Lights the stack they would go into; pinning opens a gap
                // where the new icon would go.
                d.set_drop_stack(match &a {
                    DropAction::Stack(p) => Some(p.clone()),
                    _ => None,
                });
                d.set_drop_gap(match a {
                    DropAction::Pin(at) => Some(at),
                    _ => None,
                });
                e
            })
            .unwrap_or(DROPEFFECT_NONE)
        })
    }
}

impl IDropTarget_Impl for DockDrop_Impl {
    fn DragEnter(&self, data: windows_core::Ref<IDataObject>, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> windows_core::Result<()> {
        // Each dock window has its own target: act on that dock.
        let _ctx = super::views::enter(self.hwnd.0 as isize);
        super::views::set_active(self.hwnd.0 as isize);
        let files = data.as_ref().map(|d| unsafe { files_of(d) }).unwrap_or_default();
        // Which of them are folders is asked on a helper thread (shell
        // attributes; a dead share or slow drive only stalls that thread).
        // Until an answer is in, a path counts as "not a folder".
        let unknown: Vec<String> =
            files.iter().filter(|f| crate::stacks::could_be_folder(f) && crate::stacks::known_kind(f).is_none()).cloned().collect();
        crate::stacks::check_kinds(unknown, |_| {});
        DROP_FILES.with(|f| *f.borrow_mut() = files);
        DROP_ALLOWED.with(|a| a.set(if effect.is_null() { 0 } else { unsafe { (*effect).0 } }));
        DROP_KEYS.with(|k| k.set(keys.0));
        DROP_RIGHT.with(|r| r.set(keys.0 & crate::stacks::KEY_RIGHT));
        DROP_LAST.with(|l| l.set((pt.x, pt.y)));
        with_dock(|d| d.ole_drag = true);
        let e = self.effect_at(pt, true);
        if let (Some(h), Some(d)) = (&self.helper, data.as_ref()) {
            unsafe {
                let _ = h.DragEnter(self.hwnd, d, &POINT { x: pt.x, y: pt.y }, e);
            }
        }
        if !effect.is_null() {
            unsafe { *effect = e };
        }
        Ok(())
    }

    fn DragOver(&self, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> windows_core::Result<()> {
        let _ctx = super::views::enter(self.hwnd.0 as isize);
        DROP_KEYS.with(|k| k.set(keys.0));
        // Same spot as last time: no new hover to compute.
        let moved = DROP_LAST.with(|l| l.replace((pt.x, pt.y))) != (pt.x, pt.y);
        let e = self.effect_at(pt, moved);
        if let Some(h) = &self.helper {
            unsafe {
                let _ = h.DragOver(&POINT { x: pt.x, y: pt.y }, e);
            }
        }
        if !effect.is_null() {
            unsafe { *effect = e };
        }
        Ok(())
    }

    fn DragLeave(&self) -> windows_core::Result<()> {
        let _ctx = super::views::enter(self.hwnd.0 as isize);
        if let Some(h) = &self.helper {
            unsafe {
                let _ = h.DragLeave();
            }
        }
        DROP_FILES.with(|f| f.borrow_mut().clear());
        with_dock(|d| {
            d.ole_drag = false;
            d.track_file_hover(POINTL::default(), false);
            d.set_drop_gap(None);
            d.set_drop_stack(None);
            d.on_mouse_leave();
        });
        Ok(())
    }

    fn Drop(&self, data: windows_core::Ref<IDataObject>, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> windows_core::Result<()> {
        let _ctx = super::views::enter(self.hwnd.0 as isize);
        let files = DROP_FILES.with(|f| std::mem::take(&mut *f.borrow_mut()));
        let pins = pin_candidates(&files);
        let source = DROPEFFECT(DROP_ALLOWED.with(|a| a.get()));
        // The button is up by now: the right-drag state is from DragEnter.
        let keys = keys.0 | DROP_RIGHT.with(|r| r.get());
        let (action, shown) = with_dock(|d| d.drop_action(&files, &pins, keys, source, *pt)).unwrap_or((DropAction::Nothing, DROPEFFECT_NONE));
        if let (Some(h), Some(d)) = (&self.helper, data.as_ref()) {
            unsafe {
                let _ = h.Drop(d, &POINT { x: pt.x, y: pt.y }, shown);
            }
        }
        let e = with_dock(|d| {
            d.ole_drag = false;
            d.track_file_hover(POINTL::default(), false);
            d.set_drop_gap(None);
            d.set_drop_stack(None);
            let e = if shown != DROPEFFECT_NONE && !d.drop_files(action, files, pins, shown) { DROPEFFECT_NONE } else { shown };
            // Moved into a stack by us: an optimized move, told the source.
            if shown == DROPEFFECT_MOVE && e == DROPEFFECT_NONE && d.drop_stack_hit(*pt) {
                if let Some(data) = data.as_ref() {
                    unsafe { set_performed_effect(data, DROPEFFECT_MOVE.0) };
                }
            }
            d.on_mouse_leave();
            e
        })
        .unwrap_or(DROPEFFECT_NONE);
        if !effect.is_null() {
            unsafe { *effect = e };
        }
        Ok(())
    }
}

/// Makes the dock window accept dropped files.
pub(super) unsafe fn register(hwnd: HWND) {
    let _ = windows::Win32::System::Ole::OleInitialize(None);
    let target: IDropTarget = DockDrop { hwnd, helper: drop_helper() }.into();
    if let Err(e) = windows::Win32::System::Ole::RegisterDragDrop(hwnd, &target) {
        crate::log!("drop: cannot register: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_by_centers() {
        let c = [10.0, 30.0, 50.0];
        assert_eq!(insert_index(&c, 0.0), 0);
        assert_eq!(insert_index(&c, 31.0), 2);
        assert_eq!(insert_index(&c, 99.0), 3);
    }

    #[test]
    fn pins_merge_keeps_outside_changes() {
        let p = |s: &str| PinEntry::new(s.into(), s.into());
        // We reordered A,B -> B,A; meanwhile C was added and D removed on disk.
        let latest = vec![p("A"), p("C")];
        let ours = vec![p("B"), p("A")];
        let out: Vec<String> = merge_pins(&latest, &ours, "B").into_iter().map(|e| e.path).collect();
        assert_eq!(out, vec!["B", "A", "C"]);
        let out: Vec<String> = merge_pins(&latest, &[p("D"), p("A")], "A").into_iter().map(|e| e.path).collect();
        assert_eq!(out, vec!["A", "C"], "D was removed elsewhere");
    }

    #[test]
    fn effects_respect_the_source() {
        assert_eq!(allowed(DROPEFFECT_LINK, DROPEFFECT(DROPEFFECT_LINK.0 | DROPEFFECT_COPY.0)), DROPEFFECT_LINK);
        assert_eq!(allowed(DROPEFFECT_LINK, DROPEFFECT_COPY), DROPEFFECT_COPY);
        assert_eq!(allowed(DROPEFFECT_MOVE, DROPEFFECT_COPY), DROPEFFECT_NONE);
    }

    #[test]
    fn only_plain_full_paths_are_recycled() {
        assert!(safe_to_recycle("C:\\Users\\me\\a.txt"));
        assert!(safe_to_recycle("\\\\server\\share\\a.txt"));
        assert!(!safe_to_recycle("a.txt"));
        assert!(!safe_to_recycle("C:\\temp\\*"));
        assert!(!safe_to_recycle("C:\\"));
        assert!(!safe_to_recycle("\\\\?\\C:\\x"));
        assert!(!safe_to_recycle(""));
    }

    /// A 48 DIP dock: bar 64 deep, its far edge at v = 65 in a 135 deep window.
    fn bar() -> Rect {
        Rect { x: 100.0, y: 65.0, w: 400.0, h: 64.0 }
    }
    const DEPTH: f32 = 135.0;

    #[test]
    fn remove_zone_starts_64_dip_off_the_bar() {
        let b = bar();
        assert_eq!(zone(300.0, 100.0, b, DEPTH), Zone::Dock, "on the bar");
        assert_eq!(zone(300.0, b.y - REMOVE_DIST + 1.0, b, DEPTH), Zone::Dock, "just short of it");
        assert_eq!(zone(300.0, b.y - REMOVE_DIST, b, DEPTH), Zone::Remove);
        assert_eq!(zone(300.0, -500.0, b, DEPTH), Zone::Remove, "far up the screen");
        // Pulled off past an end, but away from the dock too: still remove.
        assert_eq!(zone(0.0, -10.0, b, DEPTH), Zone::Remove);
    }

    #[test]
    fn sideways_or_past_the_edge_cancels() {
        let b = bar();
        assert_eq!(zone(b.x - 61.0, 100.0, b, DEPTH), Zone::Cancel);
        assert_eq!(zone(b.x + b.w + 61.0, 100.0, b, DEPTH), Zone::Cancel);
        assert_eq!(zone(b.x - 59.0, 100.0, b, DEPTH), Zone::Dock);
        // Past the screen edge (a monitor beyond it).
        assert_eq!(zone(300.0, DEPTH + 61.0, b, DEPTH), Zone::Cancel);
    }

    #[test]
    fn remove_zone_for_every_dock_position() {
        use super::super::edge::Frame;
        let b = bar();
        let away = b.y - REMOVE_DIST - 1.0; // canonical v just inside the zone
        let near = b.y - REMOVE_DIST + 1.0;
        for pos in [Position::Bottom, Position::Left, Position::Right] {
            let f = Frame { pos, depth: DEPTH };
            // Window point for canonical (u, v): towards the desktop is up
            // (bottom), right (left dock) or left (right dock).
            let win = |u: f32, v: f32| match pos {
                Position::Bottom => (u, v),
                Position::Left => (DEPTH - v, u),
                Position::Right => (v, u),
            };
            let (x, y) = win(300.0, away);
            let (u, v) = f.from_win(x, y);
            assert_eq!(zone(u, v, b, DEPTH), Zone::Remove, "{pos:?} away");
            let (x, y) = win(300.0, near);
            let (u, v) = f.from_win(x, y);
            assert_eq!(zone(u, v, b, DEPTH), Zone::Dock, "{pos:?} near");
            let (x, y) = win(b.x - 100.0, 100.0);
            let (u, v) = f.from_win(x, y);
            assert_eq!(zone(u, v, b, DEPTH), Zone::Cancel, "{pos:?} off the end");
        }
        // Left dock: the desktop is to the right of the window (x > depth).
        let f = Frame { pos: Position::Left, depth: DEPTH };
        let (u, v) = f.from_win(DEPTH + 50.0, 300.0);
        assert_eq!(zone(u, v, b, DEPTH), Zone::Remove);
        // Right dock: to the left of the window (x < 0).
        let f = Frame { pos: Position::Right, depth: DEPTH };
        let (u, v) = f.from_win(-50.0, 300.0);
        assert_eq!(zone(u, v, b, DEPTH), Zone::Remove);
    }

    #[test]
    fn dragged_icon_dims_when_off_and_more_when_armed() {
        assert_eq!(drag_alpha(false, false), 1.0);
        assert!(drag_alpha(true, false) < 1.0);
        assert!(drag_alpha(true, true) < drag_alpha(true, false));
    }

    fn p(s: &str) -> PinEntry {
        PinEntry::new(s.into(), s.into())
    }

    fn paths(v: &[PinEntry]) -> Vec<&str> {
        v.iter().map(|e| e.path.as_str()).collect()
    }

    #[test]
    fn undo_puts_the_pin_back_at_its_index() {
        let pins = vec![p("A"), p("C")];
        assert_eq!(paths(&reinsert_pin(&pins, &p("B"), 1).unwrap()), vec!["A", "B", "C"]);
        assert_eq!(paths(&reinsert_pin(&pins, &p("B"), 0).unwrap()), vec!["B", "A", "C"]);
        // The list got shorter meanwhile: at the end.
        assert_eq!(paths(&reinsert_pin(&pins, &p("B"), 9).unwrap()), vec!["A", "C", "B"]);
        // Pinned again meanwhile (any case): nothing to do.
        assert!(reinsert_pin(&[p("a"), p("b")], &p("B"), 0).is_none());
    }

    #[test]
    fn undo_brings_back_the_dividers_that_went_with_the_pin() {
        let d = |n: u32| PinEntry::new(String::new(), format!("nopbar:divider:{n}"));
        // A | B | C: B dragged off left two dividers side by side, one went.
        let now = vec![p("A"), d(1), p("C")];
        let back = reinsert_pins(&now, &p("B"), 2, &[(d(2), 3)]).unwrap();
        assert_eq!(paths(&back), ["A", "nopbar:divider:1", "B", "nopbar:divider:2", "C"]);
        // A | B: A dragged off left the divider at the start.
        let back = reinsert_pins(&[p("B")], &p("A"), 0, &[(d(1), 1)]).unwrap();
        assert_eq!(paths(&back), ["A", "nopbar:divider:1", "B"]);
        // A divider put back by hand meanwhile: not twice.
        let back = reinsert_pins(&[d(1), p("B")], &p("A"), 0, &[(d(1), 1)]).unwrap();
        assert_eq!(back.iter().filter(|e| e.path == "nopbar:divider:1").count(), 1);
        // The pin itself is back already: nothing.
        assert!(reinsert_pins(&[p("A"), p("B")], &p("A"), 0, &[(d(1), 1)]).is_none());
        // Nothing went with it: as reinsert_pin.
        assert_eq!(reinsert_pins(&[p("A"), p("C")], &p("B"), 1, &[]), reinsert_pin(&[p("A"), p("C")], &p("B"), 1));
    }

    #[test]
    fn undo_lasts_ten_seconds() {
        let t = Instant::now();
        assert!(undo_fresh(t, t));
        assert!(undo_fresh(t, t + Duration::from_millis(9_999)));
        assert!(!undo_fresh(t, t + Duration::from_millis(10_000)));
        // A clock read before the removal is not "late".
        assert!(undo_fresh(t + Duration::from_secs(1), t));
    }

    #[test]
    fn external_drop_index_uses_the_reorder_math() {
        // Pins centered at 40, 90, 140: a drop between the 1st and 2nd goes 2nd.
        let c = [40.0, 90.0, 140.0];
        assert_eq!(insert_index(&c, 10.0), 0);
        assert_eq!(insert_index(&c, 60.0), 1);
        assert_eq!(insert_index(&c, 120.0), 2);
        assert_eq!(insert_index(&c, 400.0), 3, "past the pins (running apps, trash): last pin");
        assert_eq!(insert_index(&[], 10.0), 0, "no pins yet");
    }

    #[test]
    fn dropped_pins_go_before_their_neighbour_on_disk() {
        let latest = vec![p("A"), p("B"), p("C")];
        let out = insert_pins_before(&latest, &[p("X"), p("Y")], Some("B"));
        assert_eq!(paths(&out), vec!["A", "X", "Y", "B", "C"]);
        // After the last pin.
        assert_eq!(paths(&insert_pins_before(&latest, &[p("X")], None)), vec!["A", "B", "C", "X"]);
        // The neighbour was removed elsewhere meanwhile: at the end.
        assert_eq!(paths(&insert_pins_before(&latest, &[p("X")], Some("Q"))), vec!["A", "B", "C", "X"]);
        // Already pinned (pinned elsewhere meanwhile): not twice.
        assert_eq!(paths(&insert_pins_before(&latest, &[p("c"), p("X")], Some("A"))), vec!["X", "A", "B", "C"]);
    }

    #[test]
    fn programs_pin_and_documents_open_with_the_app() {
        // Shortcuts / programs pin even over an icon's middle.
        assert!(!opens_with(true, true, true));
        // A folder over the middle of an app opens with it; at its edge it pins.
        assert!(opens_with(true, false, true));
        assert!(!opens_with(true, false, false));
        // Nothing new to pin (documents, or pinned already): open with the app.
        assert!(opens_with(false, true, false));
        assert!(on_middle(50.0, 26.0, 48.0));
        assert!(!on_middle(30.0, 26.0, 48.0));
    }

    #[test]
    fn what_can_be_pinned() {
        assert!(pinnable("C:\\x\\App.LNK"));
        assert!(pinnable("c:\\x\\a.exe"));
        assert!(!pinnable("c:\\x\\note.txt"));
    }
}
