//! Screen reader support: a UI Automation provider tree for our
//! custom-drawn windows (the dock, the top bars, popup menus).
//!
//! Every window publishes a snapshot of what it shows (`Tree`: a root and
//! a flat list of children with names, rects and states) whenever it lays
//! out, but only while a UIA client is reading that window (it asked within
//! the last `IDLE`), so nothing is built while no screen reader runs. The
//! COM providers answer only from these snapshots, from any thread: they
//! never borrow the window's state, never block the UI thread, and once a
//! window (this lifetime of its HWND) or an item is gone they answer
//! UIA_E_ELEMENTNOTAVAILABLE.
//!
//! What a client asks for (Invoke, Expand, a context menu, focus) is queued
//! and posted to the window (`wm_action`), which runs it in its own message
//! handler, outside any UIA call: actions open modal menus and must not run
//! inside one.
//!
//! Events (structure / name / state changes, focus, menu opened / closed)
//! are raised only while UiaClientsAreListening().

// UIA's ids and methods keep their Windows names.
#![allow(non_upper_case_globals, non_snake_case)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use windows::core::{implement, interface, Error, IUnknown, IUnknown_Vtbl, IUnknownImpl, Interface, Result, BOOL, BSTR, HRESULT, PCWSTR};
use windows::Win32::Foundation::{E_INVALIDARG, E_POINTER, HWND, LPARAM, LRESULT, RECT, S_OK, WPARAM};
use windows::Win32::System::Com::SAFEARRAY;
use windows::Win32::System::Ole::{SafeArrayCreateVector, SafeArrayPutElement};
use windows::Win32::System::Variant::{VARIANT, VT_I4};
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, RegisterWindowMessageW};

// ---- the model (pure) ---------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Role {
    ToolBar,
    MenuBar,
    Menu,
    #[default]
    Button,
    MenuItem,
    Separator,
    Text,
    Slider,
    /// A group of items (a Launchpad folder, a picture-in-picture, the
    /// system stats panel and its text rows).
    Group,
    /// A text field: focusable, its Value pattern writable (SetValue comes
    /// as Act::SetValue; the text with take_value).
    Edit,
    /// A list of items.
    List,
}

impl Role {
    fn control_type(self) -> UIA_CONTROLTYPE_ID {
        match self {
            Role::ToolBar => UIA_ToolBarControlTypeId,
            Role::MenuBar => UIA_MenuBarControlTypeId,
            Role::Menu => UIA_MenuControlTypeId,
            Role::Button => UIA_ButtonControlTypeId,
            Role::MenuItem => UIA_MenuItemControlTypeId,
            Role::Separator => UIA_SeparatorControlTypeId,
            Role::Text => UIA_TextControlTypeId,
            Role::Slider => UIA_SliderControlTypeId,
            Role::Group => UIA_GroupControlTypeId,
            Role::Edit => UIA_EditControlTypeId,
            Role::List => UIA_ListControlTypeId,
        }
    }
}

/// One child of a window's tree.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Node {
    /// Stable within its window (the runtime id is made from it).
    pub key: String,
    pub role: Role,
    pub name: String,
    pub help: String,
    /// Read-only Value pattern (battery level, slider percent).
    pub value: Option<String>,
    /// Screen px.
    pub rect: RECT,
    /// ExpandCollapse pattern and its state (expanded?).
    pub expand: Option<bool>,
    /// Toggle pattern and its state (on?).
    pub toggle: Option<bool>,
    pub invoke: bool,
    /// Has a context menu (Shift+F10 / the Applications key in Narrator).
    pub menu: bool,
    pub enabled: bool,
    /// Shortcut shown with it ("Ctrl+S").
    pub accel: String,
    /// RangeValue pattern (a slider): its value, least, greatest and step.
    /// A client setting it comes as Act::SetValue (the number as text,
    /// take_value).
    pub range: Option<Range>,
    /// A protected field (a calendar's secret link): UIA's IsPassword is
    /// set, and `value` must hold only a masked stand-in, never the text.
    pub secret: bool,
    /// SelectionItem pattern and its state (selected?): an item of a list
    /// that keeps a selection (Select comes as Act::Select).
    pub selected: Option<bool>,
    /// Scrolled out of its list's view: IsOffscreen (its rect stays).
    pub hidden: bool,
    /// ScrollItem pattern (ScrollIntoView comes as Act::ScrollIntoView).
    pub scroll: bool,
    /// Never takes the keyboard focus, even in a focusable window. SetFocus
    /// on it fails.
    pub unfocusable: bool,
}

/// A slider's numbers (RangeValue pattern).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Range {
    pub value: f64,
    pub min: f64,
    pub max: f64,
    pub step: f64,
}

impl Node {
    pub fn new(key: impl Into<String>, role: Role, name: impl Into<String>, rect: RECT) -> Self {
        Self { key: key.into(), role, name: name.into(), rect, enabled: true, ..Default::default() }
    }
    pub fn help(mut self, h: impl Into<String>) -> Self {
        self.help = h.into();
        self
    }
    pub fn value(mut self, v: impl Into<String>) -> Self {
        self.value = Some(v.into());
        self
    }
    pub fn expand(mut self, open: bool) -> Self {
        self.expand = Some(open);
        self
    }
    /// A protected field (see `secret`).
    pub fn secret(mut self) -> Self {
        self.secret = true;
        self
    }
    pub fn toggle(mut self, on: bool) -> Self {
        self.toggle = Some(on);
        self
    }
    pub fn invoke(mut self) -> Self {
        self.invoke = true;
        self
    }
    pub fn menu(mut self) -> Self {
        self.menu = true;
        self
    }
    pub fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
    }
    pub fn accel(mut self, a: impl Into<String>) -> Self {
        self.accel = a.into();
        self
    }
    pub fn range(mut self, r: Range) -> Self {
        self.range = Some(r);
        self
    }
}

/// Pure: can child `n` of `t` take the keyboard focus (SetFocus works)?
/// Only in a focusable window, and never a separator, a label or a node
/// that says it does not.
pub fn child_focusable(t: &Tree, n: &Node) -> bool {
    t.focusable && !n.unfocusable && n.role != Role::Separator && n.role != Role::Text
}

/// Pure: does window `t` keep a selection (Selection pattern)? A list
/// whose items tell whether they are selected (SelectionItem).
pub fn keeps_selection(t: &Tree) -> bool {
    t.role == Role::List && t.nodes.iter().any(|n| n.selected.is_some())
}

/// Pure: is node `n` off screen (no room, or scrolled out of its list)?
pub fn node_offscreen(n: &Node) -> bool {
    offscreen(&n.rect) || n.hidden
}

/// What one window shows.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Tree {
    pub role: Role,
    pub name: String,
    pub nodes: Vec<Node>,
    /// Key of the child with the keyboard focus (dock keyboard mode, the
    /// highlighted menu row).
    pub focus: Option<String>,
    /// The window and its children can take the keyboard focus (SetFocus
    /// works: the dock, menus), but for children that say they cannot
    /// (Node::unfocusable).
    pub focusable: bool,
    /// The window holds the keyboard focus now (the dock in keyboard mode,
    /// the first menu level).
    pub active: bool,
    /// The focus is in a child of another of our windows (a submenu level):
    /// GetFocus on this root answers with that child.
    pub focus_in: Option<isize>,
}

/// What a client asked a child to do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Act {
    Invoke,
    Expand,
    Collapse,
    Toggle,
    /// Show its context menu.
    Menu,
    Focus,
    /// A client set an Edit's text (read it with take_value).
    SetValue,
    /// Not a client's: the window's snapshot went idle and is read again.
    Refresh,
    /// A list item asked to be the selection (SelectionItem).
    Select,
    /// A list item asked to be scrolled into view (ScrollItem).
    ScrollIntoView,
}

/// A difference between two snapshots that clients hear about.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    /// Children added, removed or reordered.
    Structure,
    Name(String, String, String),
    Help(String, String, String),
    Value(String, String, String),
    /// A slider's number moved.
    Range(String, f64, f64),
    Expand(String, bool),
    Toggle(String, bool),
    /// Scrolled out of view (true) or back in (a menu row).
    Offscreen(String, bool),
    Focus(String),
    /// A list item's selection changed (SelectionItem).
    Selected(String, bool),
}

/// Not shown on screen: a rect with no width (a menu row scrolled out of
/// view).
pub fn offscreen(r: &RECT) -> bool {
    r.right <= r.left
}

/// What changed from `old` to `new` (in the order clients should hear it:
/// structure first, focus last).
pub fn diff(old: &Tree, new: &Tree) -> Vec<Change> {
    let mut out = Vec::new();
    let keys = |t: &Tree| t.nodes.iter().map(|n| n.key.clone()).collect::<Vec<_>>();
    if keys(old) != keys(new) {
        out.push(Change::Structure);
    }
    for n in &new.nodes {
        let Some(o) = old.nodes.iter().find(|o| o.key == n.key) else { continue };
        if o.name != n.name {
            out.push(Change::Name(n.key.clone(), o.name.clone(), n.name.clone()));
        }
        if o.help != n.help {
            out.push(Change::Help(n.key.clone(), o.help.clone(), n.help.clone()));
        }
        if let (Some(a), Some(b)) = (&o.value, &n.value) {
            if a != b {
                out.push(Change::Value(n.key.clone(), a.clone(), b.clone()));
            }
        }
        if let (Some(a), Some(b)) = (o.range, n.range) {
            if a.value != b.value {
                out.push(Change::Range(n.key.clone(), a.value, b.value));
            }
        }
        if let (Some(a), Some(b)) = (o.expand, n.expand) {
            if a != b {
                out.push(Change::Expand(n.key.clone(), b));
            }
        }
        if let (Some(a), Some(b)) = (o.toggle, n.toggle) {
            if a != b {
                out.push(Change::Toggle(n.key.clone(), b));
            }
        }
        if node_offscreen(o) != node_offscreen(n) {
            out.push(Change::Offscreen(n.key.clone(), node_offscreen(n)));
        }
        if let (Some(a), Some(b)) = (o.selected, n.selected) {
            if a != b {
                out.push(Change::Selected(n.key.clone(), b));
            }
        }
    }
    if let Some(f) = &new.focus {
        if old.focus.as_ref() != Some(f) && new.nodes.iter().any(|n| &n.key == f) {
            out.push(Change::Focus(f.clone()));
        }
    }
    out
}

/// Runtime id part for a key (FNV-1a, kept positive).
pub fn key_id(key: &str) -> i32 {
    let mut h: u32 = 0x811C_9DC5;
    for b in key.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    (h & 0x7FFF_FFFF) as i32
}

/// A rect in window DIPs to screen px (`origin`: the window's top-left in
/// px). Edges are rounded outward-safe the way the minimize rects are.
pub fn px_rect(origin: (i32, i32), scale: f32, x: f32, y: f32, w: f32, h: f32) -> RECT {
    RECT {
        left: origin.0 + (x * scale).round() as i32,
        top: origin.1 + (y * scale).round() as i32,
        right: origin.0 + ((x + w) * scale).round() as i32,
        bottom: origin.1 + ((y + h) * scale).round() as i32,
    }
}

/// Puts children in reading order: left to right, top to bottom for
/// equal lefts (a stable sort, so equal rects keep their order).
pub fn reading_order(nodes: &mut [Node]) {
    nodes.sort_by_key(|n| (n.rect.left, n.rect.top));
}

/// "&File" -> "File", "Save && quit" -> "Save & quit".
pub fn strip_mnemonic(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '&' {
            if it.peek() == Some(&'&') {
                out.push('&');
                it.next();
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Parts joined with the language's list separator, empty parts skipped.
pub fn join(parts: &[String]) -> String {
    let sep = crate::i18n::t("a11y.list_sep");
    parts.iter().filter(|p| !p.is_empty()).cloned().collect::<Vec<_>>().join(sep)
}

/// What a dock icon's help text says: "Not responding", "Running, 2
/// windows", "3 new", "40% done", "Folder".
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AppState {
    pub windows: usize,
    pub hung: bool,
    /// A badge count ("3", "99+"), or Some("") for a badge without one.
    pub badge: Option<String>,
    /// 0..1 while the app reports progress.
    pub progress: Option<f32>,
    pub stack: bool,
}

pub fn app_help(s: &AppState) -> String {
    use crate::i18n::{t, tf};
    let mut parts = Vec::new();
    if s.stack {
        parts.push(t("a11y.dock.stack").to_string());
    }
    if s.hung {
        parts.push(t("a11y.dock.not_responding").to_string());
    }
    match s.windows {
        0 => {}
        1 => parts.push(t("a11y.dock.running").to_string()),
        n if s.stack => parts.push(tf("a11y.dock.running_n", &[("n", &n.to_string())])),
        n => {
            parts.push(tf("a11y.dock.running_n", &[("n", &n.to_string())]));
            // The keyboard way to one of them (the cards a click shows).
            parts.push(t("a11y.dock.choose_window").to_string());
        }
    }
    match s.badge.as_deref() {
        None => {}
        Some("") => parts.push(t("a11y.dock.badge").to_string()),
        Some(n) => parts.push(tf("a11y.dock.badge_n", &[("n", n)])),
    }
    if let Some(p) = s.progress.filter(|p| *p > 0.0) {
        let pct = (p.clamp(0.0, 1.0) * 100.0).round() as u32;
        parts.push(tf("a11y.dock.progress", &[("p", &pct.to_string())]));
    }
    join(&parts)
}

/// "Battery 80%" / "Battery 80%, charging".
pub fn battery_name(percent: u8, charging: bool) -> String {
    let key = if charging { "a11y.topbar.battery_charging" } else { "a11y.topbar.battery" };
    crate::i18n::tf(key, &[("p", &percent.to_string())])
}

// ---- the registry ---------------------------------------------------------------
//
// One entry per window a client has asked for (WM_GETOBJECT), process-wide:
// UIA calls providers from its own threads. An entry lives from the first
// request until WM_DESTROY and has a generation of its own, so providers
// made for an earlier window with the same (reused) HWND stay dead.

/// A snapshot no client has touched for this long is no longer kept up to
/// date (the window stops building it); the next request refreshes it.
const IDLE: Duration = Duration::from_secs(30);

/// A COM pointer kept in the registry (our own providers, which are
/// free-threaded: they only touch this registry and post messages).
struct Shared(IUnknown);
// SAFETY: the objects behind these pointers hold plain data, take the
// registry's lock for everything else and never touch thread-bound state.
unsafe impl Send for Shared {}

struct Win {
    hwnd: isize,
    generation: u64,
    /// WM_DESTROY started: nothing new is handed out or stored.
    closing: bool,
    tree: Option<Tree>,
    root: Option<Shared>,
    /// Child providers handed out, by key (one object per child: stable
    /// identity, and they can all be disconnected).
    elems: Vec<(String, Shared)>,
    last_use: Instant,
}

static WINS: Mutex<Vec<Win>> = Mutex::new(Vec::new());
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

/// Requests waiting for their window: (window, generation, key, what).
static ACTIONS: Mutex<Vec<(isize, u64, String, Act)>> = Mutex::new(Vec::new());

static WM_ACTION: AtomicU32 = AtomicU32::new(0);

/// Text a client gave an Edit or a slider (SetValue): (window, generation,
/// key, text), newest per child.
static VALUES: Mutex<Vec<(isize, u64, String, String)>> = Mutex::new(Vec::new());

/// The text a client set on child `key` of `hwnd` (after Act::SetValue),
/// taken once. Only for the window's current tree generation.
pub fn take_value(hwnd: HWND, key: &str) -> Option<String> {
    let me = hwnd.0 as isize;
    let current = wins().iter().find(|w| w.hwnd == me && !w.closing).map(|w| w.generation);
    let mut v = VALUES.lock().unwrap_or_else(|e| e.into_inner());
    let at = v.iter().position(|(h, g, k, _)| *h == me && Some(*g) == current && k == key)?;
    Some(v.remove(at).3)
}

fn wins() -> MutexGuard<'static, Vec<Win>> {
    WINS.lock().unwrap_or_else(|e| e.into_inner())
}

fn actions() -> MutexGuard<'static, Vec<(isize, u64, String, Act)>> {
    ACTIONS.lock().unwrap_or_else(|e| e.into_inner())
}

fn live(g: &mut [Win], hwnd: isize, generation: u64) -> Option<&mut Win> {
    g.iter_mut().find(|w| w.hwnd == hwnd && w.generation == generation && !w.closing)
}

/// Makes sure `hwnd` has an entry (a client is asking); its generation.
fn ensure_entry(hwnd: isize) -> Option<u64> {
    let mut g = wins();
    if let Some(w) = g.iter_mut().find(|w| w.hwnd == hwnd) {
        if w.closing {
            return None;
        }
        w.last_use = Instant::now();
        return Some(w.generation);
    }
    let generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
    g.push(Win { hwnd, generation, closing: false, tree: None, root: None, elems: Vec::new(), last_use: Instant::now() });
    Some(generation)
}

/// Gives a window an entry so it can publish (a new menu level of a menu a
/// client is reading).
pub fn ensure(hwnd: HWND) {
    ensure_entry(hwnd.0 as isize);
}

/// `hwnd` should publish its tree: a client asked for it recently. Free
/// (no entry, or idle) when nobody reads it.
pub fn wanted(hwnd: HWND) -> bool {
    let me = hwnd.0 as isize;
    wins().iter().any(|w| w.hwnd == me && !w.closing && w.last_use.elapsed() <= IDLE)
}

/// Runs `f` on a live entry, counting it as a use; a use after the idle time
/// also asks the window for a fresh tree. "Not available" once it is gone.
fn with_win<R>(hwnd: isize, generation: u64, f: impl FnOnce(&mut Win) -> R) -> Result<R> {
    let (r, stale) = {
        let mut g = wins();
        let w = live(&mut g, hwnd, generation).ok_or_else(not_available)?;
        let stale = w.last_use.elapsed() > IDLE;
        w.last_use = Instant::now();
        (f(w), stale)
    };
    if stale {
        request(hwnd, generation, "", Act::Refresh);
    }
    Ok(r)
}

fn with_tree<R>(hwnd: isize, generation: u64, f: impl FnOnce(&Tree) -> R) -> Result<Option<R>> {
    with_win(hwnd, generation, |w| w.tree.as_ref().map(f))
}

/// Runs `f` on child `key` (the tree and its index); "not available" once
/// it is gone.
fn with_node<R>(hwnd: isize, generation: u64, key: &str, f: impl FnOnce(&Tree, usize) -> R) -> Result<R> {
    with_win(hwnd, generation, |w| {
        let t = w.tree.as_ref()?;
        t.nodes.iter().position(|n| n.key == key).map(|i| f(t, i))
    })?
    .ok_or_else(not_available)
}

fn listening() -> bool {
    unsafe { UiaClientsAreListening().as_bool() }
}

/// The message a window gets when a client asked one of its children to do
/// something (see `take`).
pub fn wm_action() -> u32 {
    let m = WM_ACTION.load(Ordering::Relaxed);
    if m != 0 {
        return m;
    }
    let m = unsafe { RegisterWindowMessageW(windows::core::w!("NopBar.A11yAction")) };
    WM_ACTION.store(m, Ordering::Relaxed);
    m
}

/// The requests waiting for `hwnd` (oldest first); requests made through
/// providers of an earlier window with the same handle are dropped.
pub fn take(hwnd: HWND) -> Vec<(String, Act)> {
    let me = hwnd.0 as isize;
    let current = wins().iter().find(|w| w.hwnd == me && !w.closing).map(|w| w.generation);
    let mut q = actions();
    let (mine, rest): (Vec<_>, Vec<_>) = q.drain(..).partition(|(h, ..)| *h == me);
    *q = rest;
    mine.into_iter().filter(|(_, g, ..)| Some(*g) == current).map(|(_, _, k, a)| (k, a)).collect()
}

fn request(hwnd: isize, generation: u64, key: &str, act: Act) {
    {
        let mut q = actions();
        // One refresh in the queue is enough.
        if act == Act::Refresh && q.iter().any(|(h, g, _, a)| *h == hwnd && *g == generation && *a == Act::Refresh) {
            return;
        }
        // Bounded: a client hammering a stuck window cannot grow it.
        if q.len() >= 64 {
            q.remove(0);
        }
        q.push((hwnd, generation, key.to_string(), act));
    }
    unsafe {
        let _ = PostMessageW(Some(HWND(hwnd as *mut _)), wm_action(), WPARAM(0), LPARAM(0));
    }
}

/// Equal but for where the children are (icons magnifying): nothing a
/// client hears about. A child going offscreen or coming back (a menu row
/// scrolled out or in) is heard about.
fn same_but_rects(a: &Tree, b: &Tree) -> bool {
    a.role == b.role
        && a.name == b.name
        && a.focus == b.focus
        && a.focus_in == b.focus_in
        && a.focusable == b.focusable
        && a.active == b.active
        && a.nodes.len() == b.nodes.len()
        && a.nodes.iter().zip(&b.nodes).all(|(x, y)| Node { rect: y.rect, ..x.clone() } == *y && offscreen(&x.rect) == offscreen(&y.rect))
}

/// Replaces `hwnd`'s snapshot (only if a client has asked for the window:
/// see `wanted`) and tells listening clients what changed.
pub fn publish(hwnd: HWND, tree: Tree) {
    let me = hwnd.0 as isize;
    let (changes, generation, dropped) = {
        let mut g = wins();
        let Some(w) = g.iter_mut().find(|w| w.hwnd == me && !w.closing) else { return };
        let Some(old) = w.tree.as_mut() else {
            w.tree = Some(tree);
            return;
        };
        if *old == tree {
            return;
        }
        if same_but_rects(old, &tree) {
            *old = tree;
            return;
        }
        let changes = if listening() { diff(old, &tree) } else { Vec::new() };
        // Children that went away: their providers are disconnected.
        let (keep, dropped): (Vec<_>, Vec<_>) = std::mem::take(&mut w.elems).into_iter().partition(|(k, _)| tree.nodes.iter().any(|n| &n.key == k));
        w.elems = keep;
        w.tree = Some(tree);
        (changes, w.generation, dropped)
    };
    // Outside the lock: UIA may call back into the providers.
    for (_, p) in dropped {
        disconnect(&p);
    }
    raise(me, generation, changes);
}

fn disconnect(p: &Shared) {
    if let Ok(s) = p.0.cast::<IRawElementProviderSimple>() {
        unsafe {
            let _ = UiaDisconnectProvider(&s);
        }
    }
}

fn raise(hwnd: isize, generation: u64, changes: Vec<Change>) {
    for c in changes {
        let r = (|| -> Result<()> {
            unsafe {
                match c {
                    Change::Structure => {
                        let Some(root) = root_simple(hwnd) else { return Ok(()) };
                        UiaRaiseStructureChangedEvent(&root, StructureChangeType_ChildrenInvalidated, std::ptr::null_mut(), 0)
                    }
                    Change::Name(k, a, b) => prop(hwnd, generation, &k, UIA_NamePropertyId, VARIANT::from(a.as_str()), VARIANT::from(b.as_str())),
                    Change::Help(k, a, b) => prop(hwnd, generation, &k, UIA_HelpTextPropertyId, VARIANT::from(a.as_str()), VARIANT::from(b.as_str())),
                    Change::Value(k, a, b) => prop(hwnd, generation, &k, UIA_ValueValuePropertyId, VARIANT::from(a.as_str()), VARIANT::from(b.as_str())),
                    Change::Range(k, a, b) => prop(hwnd, generation, &k, UIA_RangeValueValuePropertyId, VARIANT::from(a), VARIANT::from(b)),
                    Change::Expand(k, on) => prop(hwnd, generation, &k, UIA_ExpandCollapseExpandCollapseStatePropertyId, VARIANT::from(expand_state(!on).0), VARIANT::from(expand_state(on).0)),
                    Change::Toggle(k, on) => prop(hwnd, generation, &k, UIA_ToggleToggleStatePropertyId, VARIANT::from(toggle_state(!on).0), VARIANT::from(toggle_state(on).0)),
                    Change::Offscreen(k, on) => prop(hwnd, generation, &k, UIA_IsOffscreenPropertyId, VARIANT::from(!on), VARIANT::from(on)),
                    Change::Selected(k, on) => {
                        let r = prop(hwnd, generation, &k, UIA_SelectionItemIsSelectedPropertyId, VARIANT::from(!on), VARIANT::from(on));
                        if on {
                            UiaRaiseAutomationEvent(&elem_provider(hwnd, generation, &k)?.cast::<IRawElementProviderSimple>()?, UIA_SelectionItem_ElementSelectedEventId)
                        } else {
                            r
                        }
                    }
                    Change::Focus(k) => UiaRaiseAutomationEvent(&elem_provider(hwnd, generation, &k)?.cast::<IRawElementProviderSimple>()?, UIA_AutomationFocusChangedEventId),
                }
            }
        })();
        let _ = r;
    }
}

unsafe fn prop(hwnd: isize, generation: u64, key: &str, id: UIA_PROPERTY_ID, old: VARIANT, new: VARIANT) -> Result<()> {
    let p: IRawElementProviderSimple = elem_provider(hwnd, generation, key)?.cast()?;
    UiaRaiseAutomationPropertyChangedEvent(&p, id, &old, &new)
}

/// A popup menu (level) appeared or is about to go: Narrator says so.
pub fn menu_event(hwnd: HWND, opened: bool) {
    if !listening() {
        return;
    }
    let Some(root) = root_simple(hwnd.0 as isize) else { return };
    let id = if opened { UIA_MenuOpenedEventId } else { UIA_MenuClosedEventId };
    unsafe {
        let _ = UiaRaiseAutomationEvent(&root, id);
    }
}

/// WM_GETOBJECT: our provider for UIA's own request, None for everything
/// else and for a window being destroyed (the caller passes those to
/// DefWindowProc). `refresh` publishes the window's tree.
pub fn on_getobject(hwnd: HWND, wp: WPARAM, lp: LPARAM, refresh: impl FnOnce()) -> Option<LRESULT> {
    if lp.0 as i32 != UiaRootObjectId {
        return None;
    }
    ensure_entry(hwnd.0 as isize)?;
    refresh();
    let root = root_simple(hwnd.0 as isize)?;
    Some(unsafe { UiaReturnRawElementProvider(hwnd, wp, lp, &root) })
}

/// The window is going away (WM_DESTROY). It is marked closing first (its
/// providers answer "not available", nothing new is handed out even to a
/// re-entered WM_GETOBJECT), then every provider handed out is
/// disconnected, then the entry goes.
pub fn forget(hwnd: HWND) {
    let me = hwnd.0 as isize;
    let taken = {
        let mut g = wins();
        g.iter_mut().find(|w| w.hwnd == me && !w.closing).map(|w| {
            w.closing = true;
            (w.root.take(), std::mem::take(&mut w.elems))
        })
    };
    actions().retain(|(h, ..)| *h != me);
    if let Some((root, elems)) = taken {
        for (_, p) in &elems {
            disconnect(p);
        }
        if let Some(r) = &root {
            disconnect(r);
            unsafe {
                UiaReturnRawElementProvider(hwnd, WPARAM(0), LPARAM(0), None::<&IRawElementProviderSimple>);
            }
        }
        drop((root, elems));
    }
    wins().retain(|w| w.hwnd != me);
}

/// The window's root provider (made once per window lifetime).
fn root_unknown(hwnd: isize) -> Option<IUnknown> {
    let mut g = wins();
    let w = g.iter_mut().find(|w| w.hwnd == hwnd && !w.closing)?;
    if w.root.is_none() {
        let r: IRawSimple = Root { hwnd, generation: w.generation }.into();
        w.root = Some(Shared(r.cast().ok()?));
    }
    w.root.as_ref().map(|s| s.0.clone())
}

fn root_simple(hwnd: isize) -> Option<IRawElementProviderSimple> {
    root_unknown(hwnd)?.cast().ok()
}

/// Child `key`'s provider (one per child per window lifetime).
fn elem_provider(hwnd: isize, generation: u64, key: &str) -> Result<IUnknown> {
    let mut g = wins();
    let w = live(&mut g, hwnd, generation).ok_or_else(not_available)?;
    if let Some((_, p)) = w.elems.iter().find(|(k, _)| k == key) {
        return Ok(p.0.clone());
    }
    let e: IRawSimple2 = Elem { hwnd, generation, key: key.to_string() }.into();
    let u: IUnknown = e.cast()?;
    w.elems.push((key.to_string(), Shared(u.clone())));
    Ok(u)
}

fn not_available() -> Error {
    Error::from(HRESULT(UIA_E_ELEMENTNOTAVAILABLE as i32))
}

fn invalid_operation() -> Error {
    Error::from(HRESULT(UIA_E_INVALIDOPERATION as i32))
}

fn expand_state(open: bool) -> ExpandCollapseState {
    if open { ExpandCollapseState_Expanded } else { ExpandCollapseState_Collapsed }
}

fn toggle_state(on: bool) -> ToggleState {
    if on { ToggleState_On } else { ToggleState_Off }
}

fn runtime_id(key: &str) -> *mut SAFEARRAY {
    unsafe {
        let sa = SafeArrayCreateVector(VT_I4, 0, 2);
        if sa.is_null() {
            return sa;
        }
        for (i, v) in [UiaAppendRuntimeId as i32, key_id(key)].iter().enumerate() {
            let idx = i as i32;
            let _ = SafeArrayPutElement(sa, &idx, v as *const i32 as *const _);
        }
        sa
    }
}

fn uia_rect(r: RECT) -> UiaRect {
    UiaRect { left: r.left as f64, top: r.top as f64, width: (r.right - r.left).max(0) as f64, height: (r.bottom - r.top).max(0) as f64 }
}

// ---- the providers ----------------------------------------------------------------
//
// The provider interfaces are declared here with their raw signatures (same
// IIDs and vtables as Windows'), so "no element" / "no pattern" is a real
// S_OK with a NULL written to the out pointer, and every failure writes
// NULL too: the generated windows-rs shims leave the out pointer untouched
// on Err, which is not a valid NULL answer.

#[interface("d6dd68d1-86fd-4332-8666-9abedea2d24c")]
unsafe trait IRawSimple: IUnknown {
    fn ProviderOptions(&self, ret: *mut ProviderOptions) -> HRESULT;
    fn GetPatternProvider(&self, id: UIA_PATTERN_ID, ret: *mut *mut c_void) -> HRESULT;
    fn GetPropertyValue(&self, id: UIA_PROPERTY_ID, ret: *mut VARIANT) -> HRESULT;
    fn HostRawElementProvider(&self, ret: *mut *mut c_void) -> HRESULT;
}

#[interface("a0a839a9-8da1-4a82-806a-8e0d44e79f56")]
unsafe trait IRawSimple2: IRawSimple {
    fn ShowContextMenu(&self) -> HRESULT;
}

#[interface("f7063da8-8359-439c-9297-bbc5299a7d87")]
unsafe trait IRawFragment: IUnknown {
    fn Navigate(&self, dir: NavigateDirection, ret: *mut *mut c_void) -> HRESULT;
    fn GetRuntimeId(&self, ret: *mut *mut SAFEARRAY) -> HRESULT;
    fn BoundingRectangle(&self, ret: *mut UiaRect) -> HRESULT;
    fn GetEmbeddedFragmentRoots(&self, ret: *mut *mut SAFEARRAY) -> HRESULT;
    fn SetFocus(&self) -> HRESULT;
    fn FragmentRoot(&self, ret: *mut *mut c_void) -> HRESULT;
}

#[interface("620ce2a5-ab8f-40a9-86cb-de3c75599b58")]
unsafe trait IRawFragmentRoot: IUnknown {
    fn ElementProviderFromPoint(&self, x: f64, y: f64, ret: *mut *mut c_void) -> HRESULT;
    fn GetFocus(&self, ret: *mut *mut c_void) -> HRESULT;
}

/// Writes an optional interface to an out pointer (NULL for None and for
/// failures) and returns the HRESULT.
unsafe fn out_iface<T: Interface>(ret: *mut *mut c_void, r: Result<Option<T>>) -> HRESULT {
    if ret.is_null() {
        return E_POINTER;
    }
    *ret = std::ptr::null_mut();
    match r {
        Ok(Some(v)) => {
            *ret = v.into_raw();
            S_OK
        }
        Ok(None) => S_OK,
        Err(e) => e.code(),
    }
}

/// Writes a plain value (or, on failure, `empty`) to an out pointer.
unsafe fn out_val<T>(ret: *mut T, empty: T, r: Result<T>) -> HRESULT {
    if ret.is_null() {
        return E_POINTER;
    }
    match r {
        Ok(v) => {
            ret.write(v);
            S_OK
        }
        Err(e) => {
            ret.write(empty);
            e.code()
        }
    }
}

/// Child `key` of window `hwnd` as the interface `T`.
fn elem_as<T: Interface>(hwnd: isize, generation: u64, key: &str) -> Result<Option<T>> {
    elem_provider(hwnd, generation, key)?.cast().map(Some)
}

fn options() -> ProviderOptions {
    // We place the logical focus ourselves (see Act::Focus).
    ProviderOptions(ProviderOptions_ServerSideProvider.0 | ProviderOptions_ProviderOwnsSetFocus.0)
}

/// The window: a toolbar (dock), menu bar (top bar) or menu (popup).
#[implement(IRawSimple, IRawFragment, IRawFragmentRoot, ISelectionProvider)]
struct Root {
    hwnd: isize,
    generation: u64,
}

impl Root_Impl {
    /// The child that has the focus: ours, or in the window we pass it to
    /// (a submenu level).
    fn focused(&self) -> Result<Option<IRawFragment>> {
        let (own, other) = with_tree(self.hwnd, self.generation, |t| (t.focus.clone(), t.focus_in))?.unwrap_or((None, None));
        if let Some(k) = own {
            return elem_as(self.hwnd, self.generation, &k);
        }
        let Some(other) = other else { return Ok(None) };
        let target = {
            let g = wins();
            g.iter().find(|w| w.hwnd == other && !w.closing).and_then(|w| Some((w.generation, w.tree.as_ref()?.focus.clone()?)))
        };
        match target {
            Some((generation, k)) => elem_as(other, generation, &k),
            None => Ok(None),
        }
    }
}

impl IRawSimple_Impl for Root_Impl {
    unsafe fn ProviderOptions(&self, ret: *mut ProviderOptions) -> HRESULT {
        out_val(ret, ProviderOptions(0), Ok(options()))
    }

    unsafe fn GetPatternProvider(&self, id: UIA_PATTERN_ID, ret: *mut *mut c_void) -> HRESULT {
        // A list whose items can be selected keeps a selection.
        let list = id == UIA_SelectionPatternId && with_tree(self.hwnd, self.generation, keeps_selection).ok().flatten().unwrap_or(false);
        let r: Result<Option<IUnknown>> = if list { Ok(Some(self.to_interface::<ISelectionProvider>().into())) } else { Ok(None) };
        out_iface::<IUnknown>(ret, r)
    }

    unsafe fn GetPropertyValue(&self, id: UIA_PROPERTY_ID, ret: *mut VARIANT) -> HRESULT {
        let v = with_tree(self.hwnd, self.generation, |t| match id {
            UIA_ControlTypePropertyId => VARIANT::from(t.role.control_type().0),
            UIA_NamePropertyId => VARIANT::from(t.name.as_str()),
            UIA_IsKeyboardFocusablePropertyId => VARIANT::from(t.focusable),
            UIA_HasKeyboardFocusPropertyId => VARIANT::from(t.active && t.focus.is_none() && t.focus_in.is_none()),
            UIA_FrameworkIdPropertyId => VARIANT::from("NopBar"),
            _ => VARIANT::default(),
        });
        // No snapshot yet: the host's own properties (window text) stand.
        out_val(ret, VARIANT::default(), v.map(Option::unwrap_or_default))
    }

    unsafe fn HostRawElementProvider(&self, ret: *mut *mut c_void) -> HRESULT {
        let r = with_win(self.hwnd, self.generation, |_| ()).and_then(|_| UiaHostProviderFromHwnd(HWND(self.hwnd as *mut _))).map(Some);
        out_iface(ret, r)
    }
}

impl IRawFragment_Impl for Root_Impl {
    unsafe fn Navigate(&self, dir: NavigateDirection, ret: *mut *mut c_void) -> HRESULT {
        let r = (|| {
            let key = match dir {
                NavigateDirection_FirstChild => with_tree(self.hwnd, self.generation, |t| t.nodes.first().map(|n| n.key.clone()))?.flatten(),
                NavigateDirection_LastChild => with_tree(self.hwnd, self.generation, |t| t.nodes.last().map(|n| n.key.clone()))?.flatten(),
                // Parent and siblings of a hosted root come from its window.
                _ => None,
            };
            match key {
                Some(k) => elem_as::<IRawFragment>(self.hwnd, self.generation, &k),
                None => Ok(None),
            }
        })();
        out_iface(ret, r)
    }

    unsafe fn GetRuntimeId(&self, ret: *mut *mut SAFEARRAY) -> HRESULT {
        // The host window supplies the root's id.
        out_val(ret, std::ptr::null_mut(), Ok(std::ptr::null_mut()))
    }

    unsafe fn BoundingRectangle(&self, ret: *mut UiaRect) -> HRESULT {
        // The host window supplies the root's rect.
        out_val(ret, UiaRect::default(), Ok(UiaRect::default()))
    }

    unsafe fn GetEmbeddedFragmentRoots(&self, ret: *mut *mut SAFEARRAY) -> HRESULT {
        out_val(ret, std::ptr::null_mut(), Ok(std::ptr::null_mut()))
    }

    unsafe fn SetFocus(&self) -> HRESULT {
        S_OK
    }

    unsafe fn FragmentRoot(&self, ret: *mut *mut c_void) -> HRESULT {
        out_iface(ret, Ok(Some(self.to_interface::<IRawFragmentRoot>())))
    }
}

impl IRawFragmentRoot_Impl for Root_Impl {
    unsafe fn ElementProviderFromPoint(&self, x: f64, y: f64, ret: *mut *mut c_void) -> HRESULT {
        let (x, y) = (x.floor() as i32, y.floor() as i32);
        let r = (|| {
            let hit = with_tree(self.hwnd, self.generation, |t| {
                t.nodes.iter().rev().find(|n| x >= n.rect.left && x < n.rect.right && y >= n.rect.top && y < n.rect.bottom).map(|n| n.key.clone())
            })?
            .flatten();
            match hit {
                Some(k) => elem_as::<IRawFragment>(self.hwnd, self.generation, &k),
                None => Ok(None),
            }
        })();
        out_iface(ret, r)
    }

    unsafe fn GetFocus(&self, ret: *mut *mut c_void) -> HRESULT {
        out_iface(ret, self.focused())
    }
}

/// One child: a dock icon, a top bar cell, a menu row.
#[implement(IRawSimple2, IRawFragment, IInvokeProvider, IExpandCollapseProvider, IToggleProvider, IValueProvider, IRangeValueProvider, ISelectionItemProvider, IScrollItemProvider)]
struct Elem {
    hwnd: isize,
    generation: u64,
    key: String,
}

impl Elem_Impl {
    fn node<R>(&self, f: impl FnOnce(&Node) -> R) -> Result<R> {
        with_node(self.hwnd, self.generation, &self.key, |t, i| f(&t.nodes[i]))
    }

    /// Keeps `text` as what a client set on this child (take_value), then
    /// asks for Act::SetValue.
    fn set_text(&self, text: String) -> Result<()> {
        {
            let mut v = VALUES.lock().unwrap_or_else(|e| e.into_inner());
            v.retain(|(h, g, k, _)| !(*h == self.hwnd && *g == self.generation && *k == self.key));
            // Bounded like the action queue.
            if v.len() >= 16 {
                v.remove(0);
            }
            v.push((self.hwnd, self.generation, self.key.clone(), text));
        }
        self.ask(Act::SetValue)
    }

    /// Queues `act` if the child still exists (and is enabled).
    fn ask(&self, act: Act) -> Result<()> {
        let enabled = self.node(|n| n.enabled)?;
        if !enabled {
            return Err(Error::from(HRESULT(UIA_E_ELEMENTNOTENABLED as i32)));
        }
        request(self.hwnd, self.generation, &self.key, act);
        Ok(())
    }
}

impl IRawSimple_Impl for Elem_Impl {
    unsafe fn ProviderOptions(&self, ret: *mut ProviderOptions) -> HRESULT {
        out_val(ret, ProviderOptions(0), Ok(options()))
    }

    unsafe fn GetPatternProvider(&self, id: UIA_PATTERN_ID, ret: *mut *mut c_void) -> HRESULT {
        let r = self
            .node(|n| match id {
                UIA_InvokePatternId => n.invoke,
                UIA_ExpandCollapsePatternId => n.expand.is_some(),
                UIA_TogglePatternId => n.toggle.is_some(),
                UIA_ValuePatternId => n.value.is_some(),
                UIA_RangeValuePatternId => n.range.is_some(),
                UIA_SelectionItemPatternId => n.selected.is_some(),
                UIA_ScrollItemPatternId => n.scroll,
                _ => false,
            })
            .map(|has| {
                has.then(|| -> IUnknown {
                    match id {
                        UIA_InvokePatternId => self.to_interface::<IInvokeProvider>().into(),
                        UIA_ExpandCollapsePatternId => self.to_interface::<IExpandCollapseProvider>().into(),
                        UIA_TogglePatternId => self.to_interface::<IToggleProvider>().into(),
                        UIA_RangeValuePatternId => self.to_interface::<IRangeValueProvider>().into(),
                        UIA_SelectionItemPatternId => self.to_interface::<ISelectionItemProvider>().into(),
                        UIA_ScrollItemPatternId => self.to_interface::<IScrollItemProvider>().into(),
                        _ => self.to_interface::<IValueProvider>().into(),
                    }
                })
            });
        out_iface(ret, r)
    }

    unsafe fn GetPropertyValue(&self, id: UIA_PROPERTY_ID, ret: *mut VARIANT) -> HRESULT {
        let v = with_node(self.hwnd, self.generation, &self.key, |t, i| {
            let n = &t.nodes[i];
            match id {
                UIA_ControlTypePropertyId => VARIANT::from(n.role.control_type().0),
                UIA_NamePropertyId => VARIANT::from(n.name.as_str()),
                UIA_HelpTextPropertyId if !n.help.is_empty() => VARIANT::from(n.help.as_str()),
                UIA_AutomationIdPropertyId => VARIANT::from(n.key.as_str()),
                UIA_IsPasswordPropertyId => VARIANT::from(n.secret),
                UIA_IsEnabledPropertyId => VARIANT::from(n.enabled),
                UIA_IsKeyboardFocusablePropertyId => VARIANT::from(child_focusable(t, n) && n.enabled),
                UIA_HasKeyboardFocusPropertyId => VARIANT::from(t.focus.as_deref() == Some(n.key.as_str())),
                UIA_IsOffscreenPropertyId => VARIANT::from(node_offscreen(n)),
                UIA_IsControlElementPropertyId | UIA_IsContentElementPropertyId => VARIANT::from(n.role != Role::Separator),
                UIA_AcceleratorKeyPropertyId if !n.accel.is_empty() => VARIANT::from(n.accel.as_str()),
                UIA_FrameworkIdPropertyId => VARIANT::from("NopBar"),
                // "3 of 10" (separators do not count).
                UIA_PositionInSetPropertyId | UIA_SizeOfSetPropertyId if n.role != Role::Separator => {
                    let set: Vec<&Node> = t.nodes.iter().filter(|m| m.role != Role::Separator).collect();
                    if id == UIA_SizeOfSetPropertyId {
                        VARIANT::from(set.len() as i32)
                    } else {
                        VARIANT::from(set.iter().position(|m| m.key == n.key).map(|p| p as i32 + 1).unwrap_or(0))
                    }
                }
                _ => VARIANT::default(),
            }
        });
        out_val(ret, VARIANT::default(), v)
    }

    unsafe fn HostRawElementProvider(&self, ret: *mut *mut c_void) -> HRESULT {
        out_iface::<IUnknown>(ret, Ok(None))
    }
}

impl IRawSimple2_Impl for Elem_Impl {
    unsafe fn ShowContextMenu(&self) -> HRESULT {
        match self.node(|n| n.menu) {
            Ok(true) => self.ask(Act::Menu).into(),
            Ok(false) => invalid_operation().code(),
            Err(e) => e.code(),
        }
    }
}

impl IRawFragment_Impl for Elem_Impl {
    unsafe fn Navigate(&self, dir: NavigateDirection, ret: *mut *mut c_void) -> HRESULT {
        let r = (|| {
            if dir == NavigateDirection_Parent {
                with_win(self.hwnd, self.generation, |_| ())?;
                return Ok(root_unknown(self.hwnd).and_then(|r| r.cast::<IRawFragment>().ok()));
            }
            let next = with_node(self.hwnd, self.generation, &self.key, |t, i| match dir {
                NavigateDirection_NextSibling => t.nodes.get(i + 1).map(|n| n.key.clone()),
                NavigateDirection_PreviousSibling => i.checked_sub(1).and_then(|j| t.nodes.get(j)).map(|n| n.key.clone()),
                _ => None,
            })?;
            match next {
                Some(k) => elem_as::<IRawFragment>(self.hwnd, self.generation, &k),
                None => Ok(None),
            }
        })();
        out_iface(ret, r)
    }

    unsafe fn GetRuntimeId(&self, ret: *mut *mut SAFEARRAY) -> HRESULT {
        let r = self.node(|_| ()).map(|_| runtime_id(&self.key));
        out_val(ret, std::ptr::null_mut(), r)
    }

    unsafe fn BoundingRectangle(&self, ret: *mut UiaRect) -> HRESULT {
        out_val(ret, UiaRect::default(), self.node(|n| uia_rect(n.rect)))
    }

    unsafe fn GetEmbeddedFragmentRoots(&self, ret: *mut *mut SAFEARRAY) -> HRESULT {
        out_val(ret, std::ptr::null_mut(), Ok(std::ptr::null_mut()))
    }

    unsafe fn SetFocus(&self) -> HRESULT {
        let r = with_node(self.hwnd, self.generation, &self.key, |t, i| child_focusable(t, &t.nodes[i]));
        match r {
            Ok(true) => self.ask(Act::Focus).into(),
            Ok(false) => invalid_operation().code(),
            Err(e) => e.code(),
        }
    }

    unsafe fn FragmentRoot(&self, ret: *mut *mut c_void) -> HRESULT {
        let r = with_win(self.hwnd, self.generation, |_| ()).map(|_| root_unknown(self.hwnd).and_then(|r| r.cast::<IRawFragmentRoot>().ok()));
        out_iface(ret, r)
    }
}

impl ISelectionItemProvider_Impl for Elem_Impl {
    fn Select(&self) -> Result<()> {
        self.ask(Act::Select)
    }

    fn AddToSelection(&self) -> Result<()> {
        // One selection at a time: adding is selecting.
        self.ask(Act::Select)
    }

    fn RemoveFromSelection(&self) -> Result<()> {
        // A list that always keeps one card selected cannot drop it.
        Err(Error::from(HRESULT(UIA_E_INVALIDOPERATION as i32)))
    }

    fn IsSelected(&self) -> Result<BOOL> {
        self.node(|n| BOOL(n.selected.unwrap_or(false) as i32))
    }

    fn SelectionContainer(&self) -> Result<IRawElementProviderSimple> {
        root_unknown(self.hwnd).ok_or_else(not_available)?.cast()
    }
}

impl IScrollItemProvider_Impl for Elem_Impl {
    fn ScrollIntoView(&self) -> Result<()> {
        self.ask(Act::ScrollIntoView)
    }
}

impl ISelectionProvider_Impl for Root_Impl {
    fn GetSelection(&self) -> Result<*mut SAFEARRAY> {
        let keys: Vec<String> = with_tree(self.hwnd, self.generation, |t| t.nodes.iter().filter(|n| n.selected == Some(true)).map(|n| n.key.clone()).collect())?.unwrap_or_default();
        unsafe {
            let sa = SafeArrayCreateVector(windows::Win32::System::Variant::VT_UNKNOWN, 0, keys.len() as u32);
            if sa.is_null() {
                return Err(Error::from(windows::Win32::Foundation::E_OUTOFMEMORY));
            }
            for (i, k) in keys.iter().enumerate() {
                let p = elem_provider(self.hwnd, self.generation, k)?.cast::<IRawElementProviderSimple>()?;
                let idx = i as i32;
                // VT_UNKNOWN: the interface pointer itself (the array AddRefs it).
                SafeArrayPutElement(sa, &idx, p.as_raw())?;
            }
            Ok(sa)
        }
    }

    fn CanSelectMultiple(&self) -> Result<BOOL> {
        Ok(BOOL(0))
    }

    fn IsSelectionRequired(&self) -> Result<BOOL> {
        Ok(BOOL(0))
    }
}

impl IInvokeProvider_Impl for Elem_Impl {
    fn Invoke(&self) -> Result<()> {
        self.ask(Act::Invoke)
    }
}

impl IExpandCollapseProvider_Impl for Elem_Impl {
    fn Expand(&self) -> Result<()> {
        self.ask(Act::Expand)
    }

    fn Collapse(&self) -> Result<()> {
        self.ask(Act::Collapse)
    }

    fn ExpandCollapseState(&self) -> Result<ExpandCollapseState> {
        self.node(|n| match n.expand {
            Some(open) => expand_state(open),
            None => ExpandCollapseState_LeafNode,
        })
    }
}

impl IToggleProvider_Impl for Elem_Impl {
    fn Toggle(&self) -> Result<()> {
        self.ask(Act::Toggle)
    }

    fn ToggleState(&self) -> Result<ToggleState> {
        self.node(|n| toggle_state(n.toggle.unwrap_or(false)))
    }
}

impl IValueProvider_Impl for Elem_Impl {
    fn SetValue(&self, val: &PCWSTR) -> Result<()> {
        if self.node(|n| n.role)? != Role::Edit {
            return Err(invalid_operation());
        }
        let text = unsafe { val.to_string() }.map_err(|_| Error::from(E_INVALIDARG))?;
        self.set_text(text)
    }

    fn Value(&self) -> Result<BSTR> {
        self.node(|n| BSTR::from(n.value.as_deref().unwrap_or("")))
    }

    fn IsReadOnly(&self) -> Result<BOOL> {
        self.node(|n| BOOL((n.role != Role::Edit) as i32))
    }
}

impl IRangeValueProvider_Impl for Elem_Impl {
    fn SetValue(&self, val: f64) -> Result<()> {
        let r = self.node(|n| n.range)?.ok_or_else(invalid_operation)?;
        if !val.is_finite() || val < r.min || val > r.max {
            return Err(Error::from(E_INVALIDARG));
        }
        self.set_text(val.to_string())
    }

    fn Value(&self) -> Result<f64> {
        self.node(|n| n.range.map(|r| r.value).unwrap_or(0.0))
    }

    fn IsReadOnly(&self) -> Result<BOOL> {
        self.node(|n| BOOL((!n.enabled || n.range.is_none()) as i32))
    }

    fn Maximum(&self) -> Result<f64> {
        self.node(|n| n.range.map(|r| r.max).unwrap_or(0.0))
    }

    fn Minimum(&self) -> Result<f64> {
        self.node(|n| n.range.map(|r| r.min).unwrap_or(0.0))
    }

    fn LargeChange(&self) -> Result<f64> {
        self.node(|n| n.range.map(|r| r.step * 5.0).unwrap_or(0.0))
    }

    fn SmallChange(&self) -> Result<f64> {
        self.node(|n| n.range.map(|r| r.step).unwrap_or(0.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(l: i32, t: i32, rr: i32, b: i32) -> RECT {
        RECT { left: l, top: t, right: rr, bottom: b }
    }

    fn tree(nodes: Vec<Node>) -> Tree {
        Tree { role: Role::ToolBar, name: "Dock".into(), nodes, ..Default::default() }
    }

    #[test]
    fn help_text_reads_like_the_taskbar() {
        let _l = crate::i18n::test_lock("en");
        assert_eq!(app_help(&AppState::default()), "");
        assert_eq!(app_help(&AppState { windows: 1, ..Default::default() }), "Running");
        assert_eq!(app_help(&AppState { windows: 2, ..Default::default() }), "Running, 2 windows, Enter chooses a window");
        assert_eq!(
            app_help(&AppState { windows: 1, hung: true, badge: Some("3".into()), ..Default::default() }),
            "Not responding, Running, 3 new"
        );
        assert_eq!(app_help(&AppState { badge: Some(String::new()), ..Default::default() }), "New activity");
        assert_eq!(app_help(&AppState { windows: 1, progress: Some(0.404), ..Default::default() }), "Running, 40% done");
        // Progress at 0 (indeterminate) says nothing.
        assert_eq!(app_help(&AppState { progress: Some(0.0), ..Default::default() }), "");
        assert_eq!(app_help(&AppState { stack: true, ..Default::default() }), "Folder");
    }

    #[test]
    fn languages_have_their_own_list_separator() {
        // (Read from the catalogs: switching the process language here would
        // race the tests that read English texts.)
        for (tag, sep) in [("ja", "、"), ("zh-cn", "，"), ("ar", "، "), ("en", ", ")] {
            let text = std::fs::read_to_string(format!("{}/locales/{tag}.json", env!("CARGO_MANIFEST_DIR"))).unwrap();
            let m: std::collections::HashMap<String, String> = serde_json::from_str(&text).unwrap();
            assert_eq!(m["a11y.list_sep"], sep, "{tag}");
        }
    }

    #[test]
    fn battery_names() {
        let _l = crate::i18n::test_lock("en");
        assert_eq!(battery_name(80, false), "Battery 80%");
        assert_eq!(battery_name(80, true), "Battery 80%, charging");
    }

    #[test]
    fn mnemonics_are_stripped() {
        assert_eq!(strip_mnemonic("&File"), "File");
        assert_eq!(strip_mnemonic("Save && quit"), "Save & quit");
        assert_eq!(strip_mnemonic("No marks"), "No marks");
        assert_eq!(strip_mnemonic("trailing&"), "trailing");
    }

    #[test]
    fn rects_convert_to_screen_px() {
        // 150 %: a 48 DIP icon at (10, 4) in a window at (100, 2000).
        assert_eq!(px_rect((100, 2000), 1.5, 10.0, 4.0, 48.0, 48.0), r(115, 2006, 187, 2078));
        assert_eq!(px_rect((0, 0), 1.0, 0.0, 0.0, 0.0, 0.0), r(0, 0, 0, 0));
        let u = uia_rect(r(10, 20, 5, 30));
        assert_eq!((u.left, u.top, u.width, u.height), (10.0, 20.0, 0.0, 10.0), "inverted rects have no width");
    }

    #[test]
    fn children_go_in_reading_order() {
        let mut v = vec![
            Node::new("clock", Role::Button, "", r(900, 0, 1000, 24)),
            Node::new("start", Role::Button, "", r(0, 0, 30, 24)),
            Node::new("tray:a", Role::Button, "", r(500, 0, 530, 24)),
            Node::new("app", Role::MenuItem, "", r(30, 0, 120, 24)),
        ];
        reading_order(&mut v);
        let keys: Vec<&str> = v.iter().map(|n| n.key.as_str()).collect();
        assert_eq!(keys, ["start", "app", "tray:a", "clock"]);
    }

    #[test]
    fn diffs_name_what_changed() {
        let a = tree(vec![
            Node::new("app:a", Role::Button, "A", r(0, 0, 1, 1)).help("Running"),
            Node::new("app:b", Role::Button, "B", r(0, 0, 1, 1)).expand(false),
        ]);
        let mut b = a.clone();
        assert!(diff(&a, &b).is_empty());
        // Moving an icon is not a change clients need.
        b.nodes[0].rect = r(1, 1, 2, 2);
        assert!(diff(&a, &b).is_empty());
        b.nodes[0].help = "Running, 2 windows".into();
        b.nodes[1].expand = Some(true);
        b.focus = Some("app:b".into());
        assert_eq!(
            diff(&a, &b),
            vec![
                Change::Help("app:a".into(), "Running".into(), "Running, 2 windows".into()),
                Change::Expand("app:b".into(), true),
                Change::Focus("app:b".into()),
            ]
        );
        let mut c = a.clone();
        c.nodes.swap(0, 1);
        assert_eq!(diff(&a, &c), vec![Change::Structure]);
        c.nodes.pop();
        assert_eq!(diff(&a, &c)[0], Change::Structure);
        // Focus on a key that is not there is not announced.
        let mut d = a.clone();
        d.focus = Some("gone".into());
        assert!(diff(&a, &d).is_empty());
    }

    #[test]
    fn key_ids_are_stable_and_positive() {
        assert_eq!(key_id("app:c:\\x.exe"), key_id("app:c:\\x.exe"));
        assert_ne!(key_id("app:a"), key_id("app:b"));
        for k in ["", "start", "trash", "menu:0", "tray:{guid}"] {
            assert!(key_id(k) >= 0);
        }
    }

    fn h(v: isize) -> HWND {
        HWND(v as *mut _)
    }

    #[test]
    fn actions_queue_per_window() {
        // (No real windows: PostMessage to these handles just fails.)
        let a = ensure_entry(0x7001).unwrap();
        let b = ensure_entry(0x7002).unwrap();
        request(0x7001, a, "a", Act::Invoke);
        request(0x7002, b, "b", Act::Expand);
        request(0x7001, a, "c", Act::Menu);
        let mine = take(h(0x7001));
        assert_eq!(mine, vec![("a".to_string(), Act::Invoke), ("c".to_string(), Act::Menu)]);
        assert!(take(h(0x7001)).is_empty());
        assert_eq!(take(h(0x7002)).len(), 1);
        forget(h(0x7001));
        forget(h(0x7002));
    }

    #[test]
    fn a_reused_handle_does_not_revive_old_providers() {
        let old = ensure_entry(0x7101).unwrap();
        publish(h(0x7101), tree(vec![Node::new("r0", Role::MenuItem, "Old", RECT::default())]));
        assert_eq!(with_node(0x7101, old, "r0", |t, i| t.nodes[i].name.clone()).unwrap(), "Old");
        forget(h(0x7101));
        assert!(with_node(0x7101, old, "r0", |_, _| ()).is_err(), "gone with its window");
        assert!(!wanted(h(0x7101)));
        // The same handle for a new window, with a row of the same key.
        let new = ensure_entry(0x7101).unwrap();
        assert_ne!(old, new);
        publish(h(0x7101), tree(vec![Node::new("r0", Role::MenuItem, "New", RECT::default())]));
        assert_eq!(with_node(0x7101, old, "r0", |_, _| ()).unwrap_err().code(), HRESULT(UIA_E_ELEMENTNOTAVAILABLE as i32));
        assert_eq!(with_node(0x7101, new, "r0", |t, i| t.nodes[i].name.clone()).unwrap(), "New");
        // A request made through an old provider never reaches the new window.
        request(0x7101, old, "r0", Act::Invoke);
        assert!(take(h(0x7101)).is_empty());
        forget(h(0x7101));
    }

    #[test]
    fn windows_nobody_asked_for_publish_nothing() {
        publish(h(0x7201), tree(vec![Node::new("a", Role::Button, "A", RECT::default())]));
        assert!(!wanted(h(0x7201)));
        assert!(wins().iter().all(|w| w.hwnd != 0x7201));
        ensure(h(0x7201));
        assert!(wanted(h(0x7201)));
        forget(h(0x7201));
    }

    #[test]
    fn moving_children_is_not_a_change() {
        let a = tree(vec![Node::new("a", Role::Button, "A", r(0, 0, 10, 10))]);
        let mut b = a.clone();
        b.nodes[0].rect = r(5, 0, 15, 10);
        assert!(same_but_rects(&a, &b));
        b.nodes[0].name = "B".into();
        assert!(!same_but_rects(&a, &b));
        let mut c = a.clone();
        c.focus = Some("a".into());
        assert!(!same_but_rects(&a, &c));
    }

    #[test]
    fn scrolling_out_and_in_is_heard() {
        // A menu row scrolled out of view: an empty rect at the edge.
        let a = tree(vec![Node::new("r0", Role::MenuItem, "A", r(0, 16, 100, 44)), Node::new("r9", Role::MenuItem, "J", r(0, 284, 0, 284))]);
        let mut b = a.clone();
        b.nodes[0].rect = r(0, 16, 0, 16);
        b.nodes[1].rect = r(0, 256, 100, 284);
        assert!(!same_but_rects(&a, &b), "not swallowed as a move");
        assert_eq!(diff(&a, &b), vec![Change::Offscreen("r0".into(), true), Change::Offscreen("r9".into(), false)]);
        // Moving while visible is still nothing.
        let mut c = a.clone();
        c.nodes[0].rect = r(0, 20, 100, 48);
        assert!(same_but_rects(&a, &c));
        assert!(diff(&a, &c).is_empty());
        assert!(offscreen(&r(5, 0, 5, 10)) && !offscreen(&r(0, 0, 1, 1)));
    }

    #[test]
    fn raw_interfaces_match_windows() {
        assert_eq!(IRawSimple::IID, IRawElementProviderSimple::IID);
        assert_eq!(IRawSimple2::IID, IRawElementProviderSimple2::IID);
        assert_eq!(IRawFragment::IID, IRawElementProviderFragment::IID);
        assert_eq!(IRawFragmentRoot::IID, IRawElementProviderFragmentRoot::IID);
    }

    #[test]
    fn no_element_is_a_null_success() {
        unsafe {
            let mut p: *mut c_void = 0x1234 as *mut c_void;
            assert_eq!(out_iface::<IUnknown>(&mut p, Ok(None)), S_OK);
            assert!(p.is_null());
            let mut p: *mut c_void = 0x1234 as *mut c_void;
            assert_eq!(out_iface::<IUnknown>(&mut p, Err(not_available())), HRESULT(UIA_E_ELEMENTNOTAVAILABLE as i32));
            assert!(p.is_null(), "failures write NULL too");
            assert_eq!(out_iface::<IUnknown>(std::ptr::null_mut(), Ok(None)), E_POINTER);
        }
    }

    #[test]
    fn list_items_tell_their_selection_and_when_they_are_scrolled_out() {
        let r = |l, t, rr, b| RECT { left: l, top: t, right: rr, bottom: b };
        let tree = |nodes| Tree { role: Role::List, name: "List".into(), nodes, ..Default::default() };
        let item = |k: &str, rect, on, out| Node { selected: Some(on), scroll: true, hidden: out, ..Node::new(k, Role::MenuItem, k, rect) };
        let a = tree(vec![item("c1", r(0, 0, 10, 10), true, false), item("c2", r(0, 20, 10, 30), false, true)]);
        assert!(!node_offscreen(&a.nodes[0]) && node_offscreen(&a.nodes[1]), "an item scrolled out is off screen, its rect kept");
        let b = tree(vec![item("c1", r(0, 0, 10, 10), false, true), item("c2", r(0, 20, 10, 30), true, false)]);
        let d = diff(&a, &b);
        assert!(d.contains(&Change::Selected("c1".into(), false)) && d.contains(&Change::Selected("c2".into(), true)));
        assert!(d.contains(&Change::Offscreen("c1".into(), true)) && d.contains(&Change::Offscreen("c2".into(), false)));
        assert!(a.nodes[0].scroll && a.nodes[1].selected == Some(false));
        assert!(keeps_selection(&a), "a list whose items can be selected keeps a selection");
    }

    #[test]
    fn a_list_without_selectable_items_keeps_no_selection() {
        let r = RECT { left: 0, top: 0, right: 10, bottom: 10 };
        let list = Tree { role: Role::List, name: "List".into(), nodes: vec![Node { scroll: true, ..Node::new("c1", Role::MenuItem, "One", r).invoke() }], focusable: true, ..Default::default() };
        assert!(!keeps_selection(&list), "no SelectionItem children: no Selection pattern");
        assert!(!keeps_selection(&Tree { role: Role::List, ..Default::default() }), "an empty list neither");
        let group = Tree { role: Role::Group, nodes: vec![Node { selected: Some(true), ..Node::new("x", Role::MenuItem, "X", r) }], ..Default::default() };
        assert!(!keeps_selection(&group), "only a list keeps a selection");
    }

    #[test]
    fn a_node_can_refuse_the_keyboard_focus_in_a_focusable_window() {
        let r = RECT { left: 0, top: 0, right: 10, bottom: 10 };
        let mut t = Tree { role: Role::List, focusable: true, ..Default::default() };
        let item = Node { unfocusable: true, ..Node::new("c1", Role::MenuItem, "One", r) };
        let button = Node::new("b", Role::Button, "B", r);
        assert!(!child_focusable(&t, &item), "an unfocusable node never takes the focus");
        assert!(child_focusable(&t, &button), "the others in a focusable window still do");
        assert!(!child_focusable(&t, &Node::new("s", Role::Separator, "", r)) && !child_focusable(&t, &Node::new("l", Role::Text, "L", r)));
        t.focusable = false;
        assert!(!child_focusable(&t, &button), "nothing in a window that is not focusable");
    }
}
