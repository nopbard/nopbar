//! Taskbar progress and overlay badges on dock icons (spike S7).
//!
//! Other apps' ITaskbarList3 state cannot be read through an API, but
//! explorer's taskbar (hidden or not) exposes it through UI Automation:
//! - progress: the button's child ProgressBar (AutomationId
//!   "ProgressIndicator") raises RangeValue.Value changes (0..1);
//! - the button itself (AutomationId "Appid: <exe path or AUMID>") changes
//!   its HelpText / ItemStatus / Name with progress and overlay icons.
//!
//! One MTA thread holds a single property-changed subscription on the
//! taskbar subtree (re-made when explorer restarts). The handler only
//! records each event's cached values in a bounded queue and wakes that
//! thread; after ~100 ms it handles the whole batch, doing the few UIA
//! lookups it needs (bounded per batch, button identities cached), updates
//! a bounded per-app map and posts one message to the dock, which matches
//! app ids to its items. Nothing runs while nothing changes. Any UIA
//! failure turns the feature off quietly.

use super::*;
use std::sync::mpsc::{channel, Sender};

/// A theme color as Direct2D wants it.
fn d2d_of(c: crate::theme::Rgba) -> windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
    windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F { r: c.r as f32 / 255.0, g: c.g as f32 / 255.0, b: c.b as f32 / 255.0, a: c.a as f32 / 255.0 }
}
use std::sync::{Mutex, OnceLock};
use std::time::Instant;
use windows::core::{implement, Ref, BSTR, GUID};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::*;

/// Posted (once until taken) when the marks changed.
pub(super) const WM_BADGES: u32 = WM_APP + 72;
/// One-shot: gather a burst of changes, then apply them.
pub(super) const BADGE_TIMER: usize = 13;
pub(super) const BADGE_COALESCE_MS: u32 = 100;
/// Bound on remembered apps (a misbehaving taskbar cannot grow it).
const MAX_APPS: usize = 64;

/// What the taskbar shows for one app.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct AppMark {
    /// 0..1 while the app reports progress (also while it is at 0 or
    /// indeterminate, which draw nothing but are not badges).
    pub progress: Option<f32>,
    pub badge: Badge,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) enum Badge {
    #[default]
    None,
    /// An overlay without a number.
    Dot,
    /// "1".."99", or "99+".
    Count(String),
    /// Ours, not the taskbar's: the app does not respond (amber "!").
    Hung,
    /// Ours: some of the app's windows are hidden to the tray
    /// (crate::traywin): a small tray glyph on a tile.
    Tray,
}

// ---- pure helpers ---------------------------------------------------------------

/// Badge for an overlay description: the first number in it (digits only,
/// capped at "99+"), a plain dot when there is text but no (non-zero)
/// number, nothing when it is empty.
pub(super) fn parse_badge(text: &str) -> Badge {
    let t = text.trim();
    if t.is_empty() {
        return Badge::None;
    }
    let digits: String = t.chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return Badge::Dot;
    }
    let plus = t.chars().skip_while(|c| !c.is_ascii_digit()).nth(digits.len()) == Some('+');
    // Long runs are certainly "a lot".
    let n: u64 = digits.trim_start_matches('0').parse().unwrap_or(if digits.chars().all(|c| c == '0') { 0 } else { u64::MAX });
    match n {
        0 => Badge::Dot,
        n if n > 99 || (n == 99 && plus) => Badge::Count("99+".into()),
        n => Badge::Count(n.to_string()),
    }
}

/// Progress ratio from a RangeValue (0..1, or 0..max on other builds).
pub(super) fn ratio(value: f64, max: f64) -> f32 {
    let r = if max > 1.0 + 1e-6 { value / max } else { value };
    if r.is_finite() { r.clamp(0.0, 1.0) as f32 } else { 0.0 }
}

/// Lowercase app key from a taskbar button's AutomationId "Appid: ...".
/// Known-folder prefixes ("{GUID}\Vendor\app.exe") are expanded with
/// `known` so they compare equal to the exe paths the dock knows.
pub(super) fn appid_key(aid: &str, known: impl Fn(&str) -> Option<String>) -> Option<String> {
    let rest = aid.trim().strip_prefix("Appid:")?.trim();
    if rest.is_empty() {
        return None;
    }
    let full = match rest.find('}') {
        Some(end) if rest.starts_with('{') => match known(&rest[1..end]) {
            Some(base) => format!("{}{}", base.trim_end_matches('\\'), &rest[end + 1..]),
            None => rest.to_string(),
        },
        _ => rest.to_string(),
    };
    Some(full.to_lowercase())
}

/// Does key `k` (from appid_key) name app `id`? A browser's web app only by
/// its AUMID: its exe is the browser's (the browser's own download
/// progress, keyed by chrome.exe, is not the web app's).
pub(super) fn id_matches(id: &AppId, k: &str) -> bool {
    let by_aumid = id.aumid.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(k));
    by_aumid || (!id.is_web_app() && id.exe.as_deref().is_some_and(|e| e.eq_ignore_ascii_case(k)))
}

/// The exe-name fallback applies only to keys that are not a full path
/// (e.g. a known-folder id we could not expand): a full path that differs
/// from ours names another program.
pub(super) fn name_fallback_ok(k: &str) -> bool {
    k.ends_with(".exe") && !crate::stacks::could_be_folder(k)
}

fn file_name(p: &str) -> &str {
    p.rsplit('\\').next().unwrap_or(p)
}

// ---- per-app state (pure transitions) ----------------------------------------------

/// What the UIA side knows about one app. The two badge sources are kept
/// apart: ItemStatus is an overlay description for sure; HelpText may be
/// one, or may be progress text whose bar has not been seen yet.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct AppState {
    pub progress: Option<f32>,
    pub status_badge: Badge,
    pub help_badge: Badge,
    /// Last event for this app (eviction order).
    pub last: Instant,
}

impl AppState {
    pub fn new() -> Self {
        Self { progress: None, status_badge: Badge::None, help_badge: Badge::None, last: Instant::now() }
    }

    /// What the dock shows.
    pub fn mark(&self) -> AppMark {
        let badge = if self.status_badge != Badge::None {
            self.status_badge.clone()
        } else if self.progress.is_none() {
            self.help_badge.clone()
        } else {
            Badge::None
        };
        AppMark { progress: self.progress, badge }
    }

    /// The progress bar reported a value: progress is on, and whatever
    /// HelpText said meanwhile was progress text, not an overlay (a "5"
    /// from "5 percent complete" goes away here, whatever came first).
    pub fn on_progress(&mut self, r: f32) {
        self.progress = Some(r);
        self.help_badge = Badge::None;
        self.last = Instant::now();
    }

    /// The button's texts changed. `child`: what its progress bar says, if
    /// it could be looked at (Some(None) = no visible bar).
    pub fn on_texts(&mut self, status: &str, help: &str, child: Option<Option<f32>>) {
        self.status_badge = parse_badge(status);
        self.last = Instant::now();
        if help.trim().is_empty() {
            // Progress ends with its text; no text, no HelpText badge.
            self.progress = None;
            self.help_badge = Badge::None;
            return;
        }
        match child {
            Some(p) => {
                self.progress = p;
                self.help_badge = if p.is_some() { Badge::None } else { parse_badge(help) };
            }
            // Bar not visible to us: known progress means progress text.
            None => self.help_badge = if self.progress.is_some() { Badge::None } else { parse_badge(help) },
        }
    }
}

/// Adds or updates `key` in a map bounded to `cap`: a new app pushes out
/// the one heard from longest ago (apps that are gone are pruned by the
/// dock; this only bounds memory).
pub(super) fn upsert(map: &mut HashMap<String, AppState>, key: &str, cap: usize, f: impl FnOnce(&mut AppState)) {
    if !map.contains_key(key) && map.len() >= cap {
        if let Some(old) = map.iter().min_by_key(|(_, s)| s.last).map(|(k, _)| k.clone()) {
            map.remove(&old);
        }
    }
    let s = map.entry(key.to_string()).or_insert_with(AppState::new);
    f(s);
    if s.mark() == AppMark::default() {
        map.remove(key);
    }
}

// ---- the UIA thread ---------------------------------------------------------------

/// Apps' state (UIA thread writes, the dock reads and prunes).
static STATE: OnceLock<Mutex<HashMap<String, AppState>>> = OnceLock::new();
/// Raw events waiting for the UIA thread (filled by the handler).
static RAW: OnceLock<Mutex<Raw>> = OnceLock::new();
static POSTED: AtomicBool = AtomicBool::new(false);
/// A Flush is on its way to the UIA thread.
static FLUSH_SENT: AtomicBool = AtomicBool::new(false);
static TARGET: AtomicIsize = AtomicIsize::new(0);
static CMD: OnceLock<Mutex<Sender<Cmd>>> = OnceLock::new();

/// Wait after the first event of a burst, so related events (value +
/// text) are handled together and lookups happen once per batch.
const BATCH_MS: u64 = 100;
/// Raw events kept per batch (later ones for new elements are dropped;
/// the next change brings them back).
const MAX_RAW: usize = 128;
/// UIA round trips (parent / child walks) per batch at most.
const MAX_WALKS: usize = 16;
/// Progress bars whose button is known (runtime id -> app key).
const MAX_PARENTS: usize = 256;

enum Cmd {
    Resubscribe,
    Flush,
}

/// UIA elements are free-threaded in the MTA (both the handler and our
/// thread are MTA): safe to hand from one to the other.
struct Elem(IUIAutomationElement);
unsafe impl Send for Elem {}

#[derive(Default)]
struct Raw {
    /// Progress bar runtime id -> (element, value, max); newest wins.
    ranges: HashMap<Vec<i32>, (Elem, f64, f64)>,
    /// App key -> (button, ItemStatus, HelpText); newest wins.
    buttons: HashMap<String, (Elem, String, String)>,
}

fn state() -> &'static Mutex<HashMap<String, AppState>> {
    STATE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn raw() -> &'static Mutex<Raw> {
    RAW.get_or_init(|| Mutex::new(Raw::default()))
}

fn notify() {
    if !POSTED.swap(true, Ordering::SeqCst) {
        let h = HWND(TARGET.load(Ordering::SeqCst) as *mut _);
        unsafe {
            let _ = PostMessageW(Some(h), WM_BADGES, WPARAM(0), LPARAM(0));
        }
    }
}

fn send(c: Cmd) {
    if let Some(tx) = CMD.get() {
        if let Ok(t) = tx.lock() {
            let _ = t.send(c);
        }
    }
}

/// Called by the dock before reading: changes after this post again.
pub(super) fn snapshot() -> HashMap<String, AppMark> {
    POSTED.store(false, Ordering::SeqCst);
    state().lock().map(|s| s.iter().map(|(k, v)| (k.clone(), v.mark())).collect()).unwrap_or_default()
}

/// Forget apps the dock no longer runs (`keep` says which keys still
/// belong to a running app). Entries heard from in the last few seconds
/// stay: the window list may not have caught up with a new app yet.
pub(super) fn prune(keep: impl Fn(&str) -> bool) -> bool {
    let Ok(mut s) = state().lock() else { return false };
    let before = s.len();
    s.retain(|k, v| keep(k) || v.last.elapsed() < std::time::Duration::from_secs(10));
    s.len() != before
}

/// Starts the UIA thread (once). `hwnd` gets WM_BADGES.
pub(super) fn start(hwnd: HWND) {
    TARGET.store(hwnd.0 as isize, Ordering::SeqCst);
    let (tx, rx) = channel::<Cmd>();
    if CMD.set(Mutex::new(tx)).is_err() {
        return;
    }
    let _ = std::thread::Builder::new().name("taskbar-uia".into()).spawn(move || unsafe {
        if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
            crate::log!("badges: no COM; taskbar progress and badges are off");
            return;
        }
        let uia: IUIAutomation = match CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) {
            Ok(u) => u,
            Err(e) => {
                crate::log!("badges: no UI Automation ({e}); taskbar progress and badges are off");
                return;
            }
        };
        let mut sub = subscribe(&uia);
        log_state(&sub);
        let mut parents: HashMap<Vec<i32>, String> = HashMap::new();
        // Sleeps here (no CPU) until an event or an explorer restart.
        while let Ok(cmd) = rx.recv() {
            match cmd {
                Cmd::Flush => {
                    std::thread::sleep(std::time::Duration::from_millis(BATCH_MS));
                    FLUSH_SENT.store(false, Ordering::SeqCst);
                    let batch = raw().lock().map(|mut r| std::mem::take(&mut *r)).unwrap_or_default();
                    if let Ok(w) = &sub {
                        process(w, batch, &mut parents);
                    }
                }
                Cmd::Resubscribe => {
                    let _ = uia.RemoveAllEventHandlers();
                    if let Ok(mut r) = raw().lock() {
                        *r = Raw::default();
                    }
                    parents.clear();
                    if let Ok(mut s) = state().lock() {
                        s.clear();
                    }
                    notify();
                    sub = subscribe(&uia);
                    log_state(&sub);
                }
            }
        }
        let _ = uia.RemoveAllEventHandlers();
    });
}

fn log_state(r: &Result<Walk>) {
    match r {
        Ok(_) => crate::log!("badges: watching the taskbar"),
        Err(e) => crate::log!("badges: cannot watch the taskbar ({e}); progress and badges are off until explorer restarts"),
    }
}

/// Explorer made a new taskbar: subscribe again.
pub(super) fn resubscribe() {
    send(Cmd::Resubscribe);
}

/// What the UIA thread needs for its lookups.
struct Walk {
    walker: IUIAutomationTreeWalker,
    cache: IUIAutomationCacheRequest,
    /// "the progress bar child" condition.
    bar: IUIAutomationCondition,
}

unsafe fn subscribe(uia: &IUIAutomation) -> Result<Walk> {
    let tray = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null())?;
    let root = uia.ElementFromHandle(tray)?;
    // Everything the handler reads comes cached with the event (no round
    // trips to explorer inside the handler).
    let cache = uia.CreateCacheRequest()?;
    for p in [
        UIA_AutomationIdPropertyId,
        UIA_NamePropertyId,
        UIA_HelpTextPropertyId,
        UIA_ItemStatusPropertyId,
        UIA_RangeValueValuePropertyId,
        UIA_RangeValueMaximumPropertyId,
        UIA_IsOffscreenPropertyId,
    ] {
        cache.AddProperty(p)?;
    }
    let bar = uia.CreatePropertyCondition(UIA_AutomationIdPropertyId, &VARIANT::from(BSTR::from("ProgressIndicator")))?;
    let handler: IUIAutomationPropertyChangedEventHandler = Handler.into();
    let props = [UIA_RangeValueValuePropertyId, UIA_HelpTextPropertyId, UIA_ItemStatusPropertyId, UIA_NamePropertyId];
    uia.AddPropertyChangedEventHandlerNativeArray(&root, TreeScope_Subtree, &cache, &handler, &props)?;
    Ok(Walk { walker: uia.RawViewWalker()?, cache, bar })
}

/// "{GUID}" (no braces) -> that known folder's path.
fn known_folder(guid: &str) -> Option<String> {
    let g = GUID::try_from(guid).ok()?;
    unsafe {
        let p = windows::Win32::UI::Shell::SHGetKnownFolderPath(&g, windows::Win32::UI::Shell::KNOWN_FOLDER_FLAG(0), None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

fn bstr(r: Result<BSTR>) -> String {
    r.map(|b| b.to_string()).unwrap_or_default()
}

unsafe fn key_of(e: &IUIAutomationElement) -> Option<String> {
    appid_key(&bstr(e.CachedAutomationId()), known_folder)
}

/// An element's runtime id (its identity across events).
unsafe fn runtime_id(e: &IUIAutomationElement) -> Option<Vec<i32>> {
    use windows::Win32::System::Ole::{SafeArrayDestroy, SafeArrayGetElement, SafeArrayGetLBound, SafeArrayGetUBound};
    let sa = e.GetRuntimeId().ok()?;
    if sa.is_null() {
        return None;
    }
    let mut v = Vec::new();
    if let (Ok(lo), Ok(hi)) = (SafeArrayGetLBound(sa, 1), SafeArrayGetUBound(sa, 1)) {
        for i in lo..=hi.min(lo + 15) {
            let mut x = 0i32;
            if SafeArrayGetElement(sa, &i, &mut x as *mut i32 as *mut _).is_ok() {
                v.push(x);
            }
        }
    }
    let _ = SafeArrayDestroy(sa);
    (!v.is_empty()).then_some(v)
}

/// Progress of a bar element from its cached values (None if hidden).
unsafe fn bar_value(e: &IUIAutomationElement) -> Option<f32> {
    if e.CachedIsOffscreen().map(|b| b.as_bool()).unwrap_or(false) {
        return None;
    }
    let v = e.GetCachedPropertyValue(UIA_RangeValueValuePropertyId).ok().and_then(|v| f64::try_from(&v).ok()).unwrap_or(0.0);
    let max = e.GetCachedPropertyValue(UIA_RangeValueMaximumPropertyId).ok().and_then(|v| f64::try_from(&v).ok()).unwrap_or(1.0);
    Some(ratio(v, max))
}

/// upsert() that notes whether what the dock shows changed.
fn apply(s: &mut HashMap<String, AppState>, key: &str, changed: &mut bool, f: impl FnOnce(&mut AppState)) {
    let (n, before) = (s.len(), s.get(key).map(|x| x.mark()).unwrap_or_default());
    upsert(s, key, MAX_APPS, f);
    *changed |= s.len() != n || s.get(key).map(|x| x.mark()).unwrap_or_default() != before;
}

/// Handles one batch: the (bounded) UIA lookups happen here, once per
/// element per batch, not once per event.
unsafe fn process(w: &Walk, batch: Raw, parents: &mut HashMap<Vec<i32>, String>) {
    let mut walks = 0usize;
    let mut changed = false;
    let Ok(mut guard) = state().lock() else { return };
    let s = &mut *guard;
    // Values first: a value in the same batch as the progress text makes
    // that text count as progress.
    for (id, (e, v, max)) in batch.ranges {
        let key = match parents.get(&id) {
            Some(k) => Some(k.clone()),
            None if walks < MAX_WALKS => {
                walks += 1;
                let k = w.walker.GetParentElementBuildCache(&e.0, &w.cache).ok().and_then(|p| key_of(&p));
                if let Some(k) = &k {
                    if parents.len() >= MAX_PARENTS {
                        parents.clear();
                    }
                    parents.insert(id, k.clone());
                }
                k
            }
            None => None,
        };
        if let Some(k) = key {
            let r = ratio(v, max);
            apply(s, &k, &mut changed, |x| x.on_progress(r));
        }
    }
    for (key, (e, status, help)) in batch.buttons {
        // Look at the bar only when the text alone is ambiguous: text, but
        // no progress known.
        let known = s.get(&key).is_some_and(|x| x.progress.is_some());
        let child = if !help.trim().is_empty() && !known && walks < MAX_WALKS {
            walks += 1;
            let first = w.walker.GetFirstChildElementBuildCache(&e.0, &w.cache).ok().filter(|c| bstr(c.CachedAutomationId()) == "ProgressIndicator");
            let found = first.or_else(|| {
                walks += 1;
                e.0.FindFirstBuildCache(TreeScope_Children, &w.bar, &w.cache).ok()
            });
            found.map(|c| bar_value(&c))
        } else {
            None
        };
        apply(s, &key, &mut changed, |x| x.on_texts(&status, &help, child));
    }
    drop(guard);
    if changed {
        notify();
    }
}

/// Records an event; the UIA thread handles it with the rest of its batch.
#[implement(IUIAutomationPropertyChangedEventHandler)]
struct Handler;

impl IUIAutomationPropertyChangedEventHandler_Impl for Handler_Impl {
    fn HandlePropertyChangedEvent(&self, sender: Ref<IUIAutomationElement>, propertyid: UIA_PROPERTY_ID, newvalue: &VARIANT) -> windows::core::Result<()> {
        let Some(e) = sender.as_ref() else { return Ok(()) };
        unsafe {
            let Ok(mut r) = raw().lock() else { return Ok(()) };
            if propertyid == UIA_RangeValueValuePropertyId {
                let Ok(v) = f64::try_from(newvalue) else { return Ok(()) };
                let max = e.GetCachedPropertyValue(UIA_RangeValueMaximumPropertyId).ok().and_then(|m| f64::try_from(&m).ok()).unwrap_or(1.0);
                let Some(id) = runtime_id(e) else { return Ok(()) };
                if r.ranges.len() < MAX_RAW || r.ranges.contains_key(&id) {
                    r.ranges.insert(id, (Elem(e.clone()), v, max));
                }
            } else {
                // Only taskbar buttons carry an app id.
                let Some(key) = key_of(e) else { return Ok(()) };
                let status = bstr(e.CachedItemStatus());
                let help = bstr(e.CachedHelpText());
                if r.buttons.len() < MAX_RAW || r.buttons.contains_key(&key) {
                    r.buttons.insert(key, (Elem(e.clone()), status, help));
                }
            }
        }
        if !FLUSH_SENT.swap(true, Ordering::SeqCst) {
            send(Cmd::Flush);
        }
        Ok(())
    }
}

// ---- on the dock ------------------------------------------------------------------

/// Visuals of an item's marks (made when first needed).
#[derive(Default)]
pub(super) struct Marks {
    /// What is shown.
    pub mark: AppMark,
    track: Option<(SpriteVisual, CompositionRoundedRectangleGeometry)>,
    fill: Option<(SpriteVisual, CompositionRoundedRectangleGeometry)>,
    /// Pill and its size relative to the icon (w, h as fractions of it).
    badge: Option<(SpriteVisual, f32, f32)>,
    /// The app is making sound (sound.rs): a speaker and its size relative
    /// to the icon.
    pub speaker: Option<(SpriteVisual, f32)>,
}

impl Marks {
    pub fn remove_from(&mut self, ch: &windows::UI::Composition::VisualCollection) {
        if let Some((v, _)) = self.track.take() {
            let _ = ch.Remove(&v);
        }
        if let Some((v, _)) = self.fill.take() {
            let _ = ch.Remove(&v);
        }
        if let Some((v, _, _)) = self.badge.take() {
            let _ = ch.Remove(&v);
        }
        if let Some((v, _)) = self.speaker.take() {
            let _ = ch.Remove(&v);
        }
        self.mark = AppMark::default();
    }

    /// Places the marks on an icon drawn at `r` (window DIPs, upright on
    /// screen for every dock edge) with opacity `alpha`; the badge and the
    /// speaker on whole device pixels at `scale` as the icon settles
    /// (`weight`: its sharp::snap_weight, 1 at rest; mark_rect).
    pub fn place(&self, r: Rect, alpha: f32, scale: f32, weight: f32) {
        if let (Some((track, tg)), Some((fill, fg)), Some(p)) = (&self.track, &self.fill, self.mark.progress) {
            // A thin bar across the lower part of the icon: a track
            // (TRACK_H on the default icon, a capsule), growing with a
            // larger or magnified icon, never thinner.
            let w = r.w * 0.72;
            let h = progress_h(r.w);
            let x = r.x + (r.w - w) / 2.0;
            let y = r.y + r.h * 0.86 - h / 2.0;
            let rad = Vector2 { X: h / 2.0, Y: h / 2.0 };
            let shown = p > 0.0;
            let _ = track.SetOffset(Vector3 { X: x, Y: y, Z: 0.0 });
            let _ = track.SetSize(Vector2 { X: w, Y: h });
            let _ = tg.SetSize(Vector2 { X: w, Y: h });
            let _ = tg.SetCornerRadius(rad);
            let fw = (w * p).max(h);
            let _ = fill.SetOffset(Vector3 { X: x, Y: y, Z: 0.0 });
            let _ = fill.SetSize(Vector2 { X: fw, Y: h });
            let _ = fg.SetSize(Vector2 { X: fw, Y: h });
            let _ = fg.SetCornerRadius(rad);
            let a = if shown { alpha } else { 0.0 };
            let _ = track.SetOpacity(a);
            let _ = fill.SetOpacity(a);
        }
        if let Some((v, fw, fh)) = &self.badge {
            // Bottom-right corner, inside the icon (Windows taskbar).
            let (w, h) = (r.w * fw, r.w * fh);
            let (x, y, w, h) = mark_rect((r.x + r.w - w, r.y + r.h - h, w, h), scale, weight);
            let _ = v.SetOffset(Vector3 { X: x, Y: y, Z: 0.0 });
            let _ = v.SetSize(Vector2 { X: w, Y: h });
            let _ = v.SetOpacity(alpha);
        }
        if let Some((v, f)) = &self.speaker {
            // Top-left corner (the badge has the other one).
            let d = r.w * f;
            let (x, y, w, h) = mark_rect((r.x - d * 0.2, r.y - d * 0.2, d, d), scale, weight);
            let _ = v.SetOffset(Vector3 { X: x, Y: y, Z: 0.0 });
            let _ = v.SetSize(Vector2 { X: w, Y: h });
            let _ = v.SetOpacity(alpha);
        }
    }
}

/// A mark's box `r` (x, y, w, h window DIPs, continuous) moved by `s` of
/// the way onto whole device pixels at `scale` (its corner and its size
/// rounded): at rest (`s` = 1, the icon's sharp::snap_weight) the badge's
/// picture - drawn at the resting size in whole pixels - sits 1:1 on the
/// pixel grid; magnified (0) it follows the icon smoothly, and between the
/// two it slides there continuously, like the icon itself (design B3
/// review P3 #3). Pure: tested.
pub(super) fn mark_rect(r: (f32, f32, f32, f32), scale: f32, s: f32) -> (f32, f32, f32, f32) {
    let k = if scale > 0.0 { scale } else { 1.0 };
    let s = s.clamp(0.0, 1.0);
    let snap = |v: f32| v + s * ((v * k).round() / k - v);
    (snap(r.0), snap(r.1), snap(r.2), snap(r.3))
}

impl Dock {
    /// The "windows in the tray" mark: the tray glyph (crate::glyphs) on a
    /// round tile, in the theme's tile, edge and ink tokens. `d`: its side
    /// (px). The one place this mark is drawn.
    fn tray_badge(&self, d: f32) -> Option<windows::UI::Composition::CompositionDrawingSurface> {
        let f = |c: crate::theme::Rgba| windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F { r: c.r as f32 / 255.0, g: c.g as f32 / 255.0, b: c.b as f32 / 255.0, a: c.a as f32 / 255.0 };
        // The tile drawn opaque over the icon (a see-through one would
        // mix with the icon under it).
        let tile = f(self.theme.tile_solid());
        let edge = f(self.theme.tile_border());
        let ink = f(self.theme.ink());
        self.gfx
            .draw(d as u32, d as u32, |dc, ox, oy| unsafe {
                let e = windows::Win32::Graphics::Direct2D::D2D1_ELLIPSE { point: Vector2 { X: ox + d / 2.0, Y: oy + d / 2.0 }, radiusX: d / 2.0 - 0.5, radiusY: d / 2.0 - 0.5 };
                dc.FillEllipse(&e, &dc.CreateSolidColorBrush(&tile, None)?);
                dc.DrawEllipse(&e, &dc.CreateSolidColorBrush(&edge, None)?, 1.0, None);
                let g = crate::glyphs::Glyph::Tray;
                crate::glyphs::draw_device(dc, g, &crate::glyphs::fitted(g, ox, oy, d, d * 0.62), ink)
            })
            .ok()
    }
}

/// Pure: the progress bar's thickness on an icon drawn `w` DIP wide.
pub(super) fn progress_h(w: f32) -> f32 {
    use crate::theme::metrics::size::TRACK_H;
    (w * TRACK_H / crate::config::DOCK_ICON_DEFAULT).max(TRACK_H)
}

/// Badge pill height as a share of the icon (before badge_metrics puts it
/// on the grid).
const BADGE_H: f32 = 0.38;

/// A badge's sizes (DIP) for an icon `icon` DIP at rest.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct BadgeMetrics {
    /// The count pill's height (its width grows with the digits).
    pub h: f32,
    /// The digits' size: one of the type ramp's.
    pub font: f32,
    /// Room each side of the digits.
    pub pad: f32,
    /// The tray and "not responding" discs.
    pub disc: f32,
    /// The plain dot.
    pub dot: f32,
}

/// Pure: a badge for an icon `icon` DIP at rest: its height on the 4 DIP
/// grid (16..=24, 20 for the default 52), the ramp size nearest 60 % of
/// it, the discs one grid step smaller, the dot two.
pub(super) fn badge_metrics(icon: f32) -> BadgeMetrics {
    use crate::theme::{metrics::space, ramp};
    let h = ((icon * BADGE_H / space::S2).round() * space::S2).clamp(16.0, 24.0);
    let want = h * 0.6;
    let font = [ramp::CAPTION.size, ramp::FOOTNOTE.size, ramp::BODY.size]
        .into_iter()
        .min_by(|a, b| (a - want).abs().total_cmp(&(b - want).abs()))
        .unwrap_or(ramp::FOOTNOTE.size);
    BadgeMetrics { h, font, pad: space::S2, disc: h - space::S2, dot: h - space::S3 }
}

impl Dock {
    /// BADGE_TIMER: the newest marks from the UIA thread.
    pub(super) fn take_marks(&mut self) {
        self.marks = snapshot();
        self.refresh_marks();
    }

    /// Which mark item `it` gets: exact app id, else a unique exe file name.
    fn mark_for(&self, i: usize) -> AppMark {
        let it = &self.items[i];
        if it.kind != Kind::App || it.windows.is_empty() || self.marks.is_empty() {
            return AppMark::default();
        }
        if let Some((_, m)) = self.marks.iter().find(|(k, _)| id_matches(&it.id, k)) {
            return m.clone();
        }
        // (A web app is never matched by its browser's exe name either.)
        let Some(exe) = it.id.exe.as_deref().filter(|_| !it.id.is_web_app()) else { return AppMark::default() };
        let name = file_name(exe);
        // Only for keys whose full path is unknown: a different full path
        // is a different app, never ours.
        let mut hits = self.marks.iter().filter(|(k, _)| name_fallback_ok(k) && file_name(k).eq_ignore_ascii_case(name));
        match (hits.next(), hits.next()) {
            (Some((_, m)), None) => {
                // Only if no other running item has that exe name too
                // (web apps running the browser's exe do not count).
                let others = self.items.iter().filter(|o| o.kind == Kind::App && !o.windows.is_empty() && !o.id.is_web_app() && o.id.exe.as_deref().map(file_name).is_some_and(|n| n.eq_ignore_ascii_case(name))).count();
                if others == 1 { m.clone() } else { AppMark::default() }
            }
            _ => AppMark::default(),
        }
    }

    /// Brings every item's marks up to date (after new marks, a new window
    /// list or rebuilt items).
    pub(super) fn refresh_marks(&mut self) {
        // Apps that are gone take their marks with them (a later app with
        // the same id starts clean).
        let running: Vec<(AppId, Option<String>)> = self
            .items
            .iter()
            .filter(|it| it.kind == Kind::App && !it.windows.is_empty())
            .map(|it| (it.id.clone(), it.id.exe.as_deref().filter(|_| !it.id.is_web_app()).map(|e| file_name(e).to_lowercase())))
            .collect();
        let keep = |k: &str| {
            running.iter().any(|(id, name)| id_matches(id, k) || (name_fallback_ok(k) && name.as_deref().is_some_and(|n| file_name(k).eq_ignore_ascii_case(n))))
        };
        if prune(keep) {
            self.marks = snapshot();
        }
        let mut changed = false;
        for i in 0..self.items.len() {
            let mut want = self.mark_for(i);
            // Not responding wins over the app's own badge.
            if self.items[i].hung {
                want.badge = Badge::Hung;
            } else if want.badge == Badge::None && !self.items[i].tray.is_empty() {
                // Windows in the tray: told only when nothing else is.
                want.badge = Badge::Tray;
            }
            if want != self.items[i].marks.mark {
                self.set_marks(i, want);
                changed = true;
            }
        }
        // The top bar's Live Island shows the same progress (with the same
        // icon brushes); it ignores a list it already has.
        crate::topbar::island::set_jobs(
            self.hwnd.0 as isize,
            self.items
                .iter()
                .filter_map(|it| {
                    let ratio = it.marks.mark.progress?;
                    Some(crate::topbar::island::JobFeed {
                        key: it.path.clone(),
                        name: it.name.clone(),
                        exe: it.id.exe.clone(),
                        ratio,
                        hwnd: it.windows.first().map(|h| h.0 as isize).unwrap_or(0),
                        icon: it.large_brush(),
                    })
                })
                .collect(),
        );
        if changed {
            self.layout();
        }
    }

    /// Theme / accent changed: draw the marks again.
    pub(super) fn reset_marks(&mut self) {
        if let Ok(ch) = self.layer.Children() {
            for it in &mut self.items {
                it.marks.remove_from(&ch);
            }
        }
        self.refresh_marks();
        self.reset_speakers();
    }

    fn set_marks(&mut self, i: usize, want: AppMark) {
        let Ok(ch) = self.layer.Children() else { return };
        let c = self.gfx.compositor.clone();
        let old = std::mem::take(&mut self.items[i].marks.mark);
        // Progress bar.
        if want.progress.is_some() {
            if self.items[i].marks.track.is_none() {
                // The kit's progress track and fill (greys; the system's
                // colors in high contrast).
                let track = bar_visual(&c, rgba(self.theme.track()));
                let fill = bar_visual(&c, rgba(self.theme.active()));
                if let (Some(t), Some(f)) = (track, fill) {
                    let _ = ch.InsertAtTop(&t.0);
                    let _ = ch.InsertAtTop(&f.0);
                    let m = &mut self.items[i].marks;
                    m.track = Some(t);
                    m.fill = Some(f);
                }
            }
        } else {
            let m = &mut self.items[i].marks;
            for v in [m.track.take(), m.fill.take()].into_iter().flatten() {
                let _ = ch.Remove(&v.0);
            }
        }
        // Badge.
        if want.badge != old.badge || self.items[i].marks.badge.is_none() {
            if let Some((v, _, _)) = self.items[i].marks.badge.take() {
                let _ = ch.Remove(&v);
            }
            if want.badge != Badge::None {
                if let Some(b) = self.badge_visual(&want.badge) {
                    let _ = ch.InsertAtTop(&b.0);
                    self.items[i].marks.badge = Some(b);
                }
            }
        }
        self.items[i].marks.mark = want;
    }

    /// A pill with digits (or a dot) in the "on" colors, no hue (design
    /// B3: achromatic in both modes, the accent is for on / selected only):
    /// light with dark ink in dark mode, the reverse in light; Highlight in
    /// high contrast. Drawn at the icon's resting size in whole pixels
    /// (badge_metrics: on the 4 DIP grid, its digits on the type ramp), so
    /// it is sharp where it sits nearly all the time.
    fn badge_visual(&self, b: &Badge) -> Option<(SpriteVisual, f32, f32)> {
        use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_SEMI_BOLD;
        let c = &self.gfx.compositor;
        let s = self.scale;
        let icon_px = self.look.icon * s;
        let m = badge_metrics(self.look.icon);
        let h = (m.h * s).round().max(1.0);
        let red = d2d_of(self.theme.active());
        let white = d2d_of(self.theme.on_active());
        let family = crate::fonts::ui_family();
        let (surface, w, hh) = match b {
            Badge::None => return None,
            Badge::Tray => {
                let d = (m.disc * s).round().max(1.0);
                (self.tray_badge(d)?, d, d)
            }
            Badge::Hung => {
                // The "on" disc with its ink "!" (no hue; Highlight and
                // HighlightText in high contrast).
                let d = (m.disc * s).round().max(1.0);
                let amber = red;
                let ink = white;
                let (layout, tw, th) = self.gfx.text_layout("!", &family, m.font * s, DWRITE_FONT_WEIGHT_SEMI_BOLD).ok()?;
                let s = self
                    .gfx
                    .draw(d as u32, d as u32, |dc, ox, oy| unsafe {
                        let br = dc.CreateSolidColorBrush(&amber, None)?;
                        let fg = dc.CreateSolidColorBrush(&ink, None)?;
                        let e = windows::Win32::Graphics::Direct2D::D2D1_ELLIPSE { point: Vector2 { X: ox + d / 2.0, Y: oy + d / 2.0 }, radiusX: d / 2.0, radiusY: d / 2.0 };
                        dc.FillEllipse(&e, &br);
                        dc.DrawTextLayout(
                            Vector2 { X: ox + ((d - tw) / 2.0).round(), Y: oy + ((d - th) / 2.0).round() },
                            &layout,
                            &fg,
                            windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                        Ok(())
                    })
                    .ok()?;
                (s, d, d)
            }
            Badge::Dot => {
                let d = (m.dot * s).round().max(1.0);
                let s = self
                    .gfx
                    .draw(d as u32, d as u32, |dc, ox, oy| unsafe {
                        let br = dc.CreateSolidColorBrush(&red, None)?;
                        let e = windows::Win32::Graphics::Direct2D::D2D1_ELLIPSE { point: Vector2 { X: ox + d / 2.0, Y: oy + d / 2.0 }, radiusX: d / 2.0, radiusY: d / 2.0 };
                        dc.FillEllipse(&e, &br);
                        Ok(())
                    })
                    .ok()?;
                (s, d, d)
            }
            Badge::Count(t) => {
                // Tabular digits: "18" and "11" are as wide.
                let (layout, tw, th) = self.gfx.text_layout_tabular(t, &family, m.font * s, DWRITE_FONT_WEIGHT_SEMI_BOLD).ok()?;
                let w = (tw + m.pad * 2.0 * s).max(h).round();
                let s = self
                    .gfx
                    .draw(w as u32, h as u32, |dc, ox, oy| unsafe {
                        let br = dc.CreateSolidColorBrush(&red, None)?;
                        let fg = dc.CreateSolidColorBrush(&white, None)?;
                        let rr = windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT {
                            rect: windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F { left: ox, top: oy, right: ox + w, bottom: oy + h },
                            radiusX: h / 2.0,
                            radiusY: h / 2.0,
                        };
                        dc.FillRoundedRectangle(&rr, &br);
                        dc.DrawTextLayout(
                            Vector2 { X: ox + ((w - tw) / 2.0).round(), Y: oy + ((h - th) / 2.0).round() },
                            &layout,
                            &fg,
                            windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
                        );
                        Ok(())
                    })
                    .ok()?;
                (s, w, h)
            }
        };
        let brush = c.CreateSurfaceBrushWithSurface(&surface).ok()?;
        let _ = brush.SetStretch(CompositionStretch::Fill);
        let v = c.CreateSpriteVisual().ok()?;
        let _ = v.SetBrush(&brush);
        let _ = v.SetOpacity(0.0);
        // Sizes relative to the icon at its largest.
        Some((v, w / icon_px, hh / icon_px))
    }
}

/// A rounded bar sprite in one color.
fn bar_visual(c: &Compositor, fill: Color) -> Option<(SpriteVisual, CompositionRoundedRectangleGeometry)> {
    let v = c.CreateSpriteVisual().ok()?;
    v.SetBrush(&c.CreateColorBrushWithColor(fill).ok()?).ok()?;
    let g = c.CreateRoundedRectangleGeometry().ok()?;
    v.SetClip(&c.CreateGeometricClipWithGeometry(&g).ok()?).ok()?;
    let _ = v.SetOpacity(0.0);
    Some((v, g))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(s: &str) -> Badge {
        Badge::Count(s.into())
    }

    #[test]
    fn badge_numbers_come_from_the_text() {
        assert_eq!(parse_badge(""), Badge::None);
        assert_eq!(parse_badge("   "), Badge::None);
        assert_eq!(parse_badge("3 unread messages"), count("3"));
        assert_eq!(parse_badge("읽지 않은 메시지 12개"), count("12"));
        assert_eq!(parse_badge("New activity"), Badge::Dot);
        assert_eq!(parse_badge("0 new"), Badge::Dot, "zero is just a mark");
        assert_eq!(parse_badge("007"), count("7"));
    }

    #[test]
    fn badge_numbers_are_capped() {
        assert_eq!(parse_badge("99"), count("99"));
        assert_eq!(parse_badge("99+ unread"), count("99+"));
        assert_eq!(parse_badge("100"), count("99+"));
        assert_eq!(parse_badge("123456789012345678901234567890"), count("99+"), "overflow");
        assert_eq!(parse_badge("9+"), count("9"));
    }

    #[test]
    fn progress_text_is_never_a_badge() {
        // HelpText first, its bar not visible to us yet: looks like "5"...
        let mut s = AppState::new();
        s.on_texts("", "작업 5.000458퍼센트 완료", None);
        assert_eq!(s.mark().badge, count("5"));
        // ...until the bar's value arrives: then it was progress text.
        s.on_progress(0.05);
        assert_eq!(s.mark(), AppMark { progress: Some(0.05), badge: Badge::None });
        // Later texts during progress stay progress text.
        s.on_texts("", "작업 10퍼센트 완료", None);
        assert_eq!(s.mark().badge, Badge::None);
        // Progress ends with its text.
        s.on_texts("", "", None);
        assert_eq!(s.mark(), AppMark::default());
    }

    #[test]
    fn badges_by_source() {
        // ItemStatus is an overlay for sure, even during progress.
        let mut s = AppState::new();
        s.on_progress(0.5);
        s.on_texts("3 new", "작업 50퍼센트 완료", None);
        assert_eq!(s.mark(), AppMark { progress: Some(0.5), badge: count("3") });
        // HelpText with no progress (the bar looked at and not there).
        let mut s = AppState::new();
        s.on_texts("", "2 unread", Some(None));
        assert_eq!(s.mark().badge, count("2"));
        // The bar is there (0 % / indeterminate): the text is progress.
        let mut s = AppState::new();
        s.on_texts("", "작업 0퍼센트 완료", Some(Some(0.0)));
        assert_eq!(s.mark(), AppMark { progress: Some(0.0), badge: Badge::None });
    }

    #[test]
    fn full_map_evicts_the_oldest_app() {
        let mut m: HashMap<String, AppState> = HashMap::new();
        for i in 0..3 {
            upsert(&mut m, &format!("app{i}"), 3, |x| x.on_progress(0.5));
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        // A fourth app is taken in; the one heard from longest ago goes.
        upsert(&mut m, "new", 3, |x| x.on_progress(0.1));
        assert_eq!(m.len(), 3);
        assert!(m.contains_key("new") && !m.contains_key("app0"));
        // Nothing to show: not kept at all.
        upsert(&mut m, "app1", 3, |x| x.on_texts("", "", None));
        assert!(!m.contains_key("app1"));
    }

    #[test]
    fn exe_name_fallback_only_without_a_full_path() {
        assert!(!name_fallback_ok(r"c:\a\client.exe"), "another full path is another app");
        assert!(!name_fallback_ok(r"\\server\x\client.exe"));
        assert!(name_fallback_ok(r"{00000000-0000-0000-0000-000000000000}\client.exe"));
        assert!(name_fallback_ok("client.exe"));
        assert!(!name_fallback_ok("microsoft.windowsterminal_8wekyb3d8bbwe!app"));
    }

    #[test]
    fn progress_ratio() {
        assert_eq!(ratio(0.25, 1.0), 0.25);
        assert_eq!(ratio(50.0, 100.0), 0.5);
        assert_eq!(ratio(1.5, 1.0), 1.0);
        assert_eq!(ratio(f64::NAN, 1.0), 0.0);
        assert_eq!(ratio(-1.0, 0.0), 0.0);
    }

    #[test]
    fn app_ids_from_the_taskbar() {
        let known = |g: &str| (g == "6D809377-6AF0-444B-8957-A3773F02200E").then(|| "C:\\Program Files".to_string());
        assert_eq!(appid_key("Appid: C:\\Apps\\Foo.exe", known).as_deref(), Some("c:\\apps\\foo.exe"));
        assert_eq!(
            appid_key("Appid: {6D809377-6AF0-444B-8957-A3773F02200E}\\Google\\Chrome\\Application\\chrome.exe", known).as_deref(),
            Some("c:\\program files\\google\\chrome\\application\\chrome.exe")
        );
        assert_eq!(appid_key("Appid: Microsoft.WindowsTerminal_8wekyb3d8bbwe!App", known).as_deref(), Some("microsoft.windowsterminal_8wekyb3d8bbwe!app"));
        // Unknown folder ids stay as they are.
        assert_eq!(appid_key("Appid: {00000000-0000-0000-0000-000000000000}\\x.exe", known).as_deref(), Some("{00000000-0000-0000-0000-000000000000}\\x.exe"));
        assert_eq!(appid_key("ProgressIndicator", known), None);
        assert_eq!(appid_key("Appid: ", known), None);
    }

    #[test]
    fn ids_match_exe_or_aumid() {
        let exe = AppId { exe: Some("c:\\apps\\foo.exe".into()), aumid: None };
        assert!(id_matches(&exe, "c:\\apps\\foo.exe"));
        assert!(!id_matches(&exe, "c:\\apps\\bar.exe"));
        let pkg = AppId { exe: Some("c:\\program files\\windowsapps\\x\\x.exe".into()), aumid: Some("Microsoft.WindowsTerminal_8wekyb3d8bbwe!App".into()) };
        assert!(id_matches(&pkg, "microsoft.windowsterminal_8wekyb3d8bbwe!app"));
        assert!(!id_matches(&AppId::default(), ""));
    }

    #[test]
    fn a_web_app_never_takes_its_browsers_marks() {
        let chrome = "c:\\program files\\google\\chrome\\application\\chrome.exe";
        let web = AppId { exe: Some(chrome.into()), aumid: Some("Chrome._crx_cmkncekebbplodngbpllndjkfo".into()) };
        // Chrome's download progress is keyed by its exe: Chrome's, not the web app's.
        assert!(!id_matches(&web, chrome));
        assert!(id_matches(&AppId { exe: Some(chrome.into()), aumid: None }, chrome));
        assert!(id_matches(&AppId { exe: Some(chrome.into()), aumid: Some("Chrome".into()) }, chrome));
        // The web app's own button (its AUMID, any case).
        assert!(id_matches(&web, "chrome._crx_cmkncekebbplodngbpllndjkfo"));
    }
    #[test]
    fn badges_sit_on_the_grid_with_ramp_digits() {
        let ramp = [crate::theme::ramp::CAPTION.size, crate::theme::ramp::FOOTNOTE.size, crate::theme::ramp::BODY.size];
        for icon in [32.0f32, 40.0, 48.0, 52.0, 64.0, 80.0, 96.0] {
            let m = badge_metrics(icon);
            for v in [m.h, m.pad, m.disc, m.dot] {
                assert_eq!(v % 4.0, 0.0, "{icon}: {v} off the grid");
            }
            assert!((16.0..=24.0).contains(&m.h));
            assert!(ramp.contains(&m.font), "{icon}: {} is not a ramp size", m.font);
            assert!(m.dot < m.disc && m.disc < m.h);
        }
        let m = badge_metrics(52.0);
        assert_eq!((m.h, m.font, m.disc, m.dot), (20.0, 12.0, 16.0, 12.0), "the default dock's");
    }

    #[test]
    fn no_badge_asks_for_bold_or_a_fixed_font() {
        let src = include_str!("badges.rs");
        let code = &src[..src.find("#[cfg(test)]").unwrap()];
        assert!(!code.contains("FONT_WEIGHT_BOLD"), "never Bold");
        assert!(!code.contains("\"Segoe UI Variable Text\""), "the user's UI font");
        assert!(!code.contains("theme.accent()"), "no hue on a badge");
    }

    #[test]
    fn a_resting_badge_sits_on_whole_device_pixels() {
        // Design B3 review P3 #3: the default 52 DIP icon at 125 %, its
        // resting square on whole pixels (65 px from px 100): the count
        // badge (20 DIP = 25 px tall) used to sit 3.75 px over it.
        let is_whole = |v: f32, k: f32| ((v * k) - (v * k).round()).abs() < 1e-3;
        for k in [1.0f32, 1.25, 1.5, 1.75, 2.0, 2.5] {
            let icon = (100.0 / k, 40.0 / k, 52.0, 52.0);
            for (fw, fh) in [(25.0 / 65.0, 25.0 / 65.0), (37.0 / 65.0, 25.0 / 65.0), (15.0 / 65.0, 15.0 / 65.0)] {
                let (w, h) = (icon.2 * fw, icon.2 * fh);
                let raw = (icon.0 + icon.2 - w * 0.75, icon.1 - h * 0.15, w, h);
                let (x, y, w2, h2) = mark_rect(raw, k, 1.0);
                for v in [x, y, w2, h2] {
                    assert!(is_whole(v, k), "scale {k}: {v} DIP is {} px", v * k);
                }
                // Within half a pixel of where it was asked to be.
                assert!((x - raw.0).abs() * k <= 0.5 + 1e-3 && (y - raw.1).abs() * k <= 0.5 + 1e-3);
                // Magnified: untouched; settling: part of the way, smoothly.
                assert_eq!(mark_rect(raw, k, 0.0), raw);
                let half = mark_rect(raw, k, 0.5);
                assert!((half.1 - (raw.1 + y) / 2.0).abs() < 1e-4, "halfway at weight 0.5");
            }
        }
        // The dock places its marks with the icon's own snap weight.
        let layout = include_str!("layout.rs");
        assert!(layout.contains("it.marks.place(r, alpha, scale, weight)"));
    }
}
