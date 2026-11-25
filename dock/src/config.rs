//! Persistent settings in %APPDATA%\NopBar\config.json (schema v2).
//!
//! v1 files (`{"pins": [...], "hide_taskbar": bool}`) are migrated on load.
//! Every section has defaults, so missing keys and unknown keys are fine.
//! A file that is not valid JSON is kept as `config.json.bak-<epoch>` and the
//! dock starts from defaults instead of failing.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::pinned;

pub const VERSION: u32 = 2;

/// Hash of the last config text this process wrote, so the file watcher can
/// tell our own saves from outside edits.
static LAST_WRITTEN: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct PinEntry {
    pub name: String,
    /// .lnk, .exe or shell:AppsFolder\<AUMID>
    pub path: String,
    /// A browser's web app: who it is, and how it starts and looks without
    /// its shortcut (webapps.rs). Written only for web apps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web: Option<crate::webapps::WebPin>,
}

impl PinEntry {
    pub fn new(name: String, path: String) -> Self {
        PinEntry { name, path, web: None }
    }

    /// A new divider, its path unlike any in `pins` (pins are told apart
    /// by path everywhere: each divider has its own).
    pub fn divider(pins: &[PinEntry]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        loop {
            let n = now.wrapping_add(NEXT.fetch_add(1, Ordering::Relaxed));
            let path = format!("{DIVIDER_PREFIX}{n:x}");
            if !pins.iter().any(|p| p.path.eq_ignore_ascii_case(&path)) {
                return PinEntry::new(String::new(), path);
            }
        }
    }

    /// A divider between pinned apps, not an app.
    pub fn is_divider(&self) -> bool {
        is_divider_path(&self.path)
    }
}

/// A divider in the pinned list is an entry with this path prefix (and an
/// empty name). Older builds read it as a pin they cannot open (an empty
/// slot), never as a broken file: an entry without "name" and "path"
/// would fail the whole list there.
pub const DIVIDER_PREFIX: &str = "nopbar:divider:";

pub fn is_divider_path(path: &str) -> bool {
    path.get(..DIVIDER_PREFIX.len()).is_some_and(|p| p.eq_ignore_ascii_case(DIVIDER_PREFIX))
}

/// Pure: which entries of a pinned list (`divider[i]`: entry i is a
/// divider) are redundant: one at the start of the list, or right after
/// another divider (the first of a run stays). One at the end stays: it
/// sets the pinned apps off from the running ones.
pub fn redundant_dividers(divider: &[bool]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < divider.len() {
        if !divider[i] {
            i += 1;
            continue;
        }
        // A run of dividers, i..end: all go at the start, else all but
        // the first.
        let end = (i..divider.len()).find(|&j| !divider[j]).unwrap_or(divider.len());
        let first = if i == 0 { i } else { i + 1 };
        out.extend(first..end);
        i = end;
    }
    out
}

/// The redundant dividers' paths in `pins` (redundant_dividers).
pub fn redundant_divider_paths(pins: &[PinEntry]) -> Vec<String> {
    let flags: Vec<bool> = pins.iter().map(PinEntry::is_divider).collect();
    redundant_dividers(&flags).into_iter().map(|i| pins[i].path.clone()).collect()
}

/// `pins` without redundant dividers (redundant_dividers).
pub fn normalize_dividers(pins: &[PinEntry]) -> Vec<PinEntry> {
    let flags: Vec<bool> = pins.iter().map(PinEntry::is_divider).collect();
    let drop = redundant_dividers(&flags);
    pins.iter().enumerate().filter(|(i, _)| !drop.contains(i)).map(|(_, p)| p.clone()).collect()
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Position {
    #[default]
    Bottom,
    Left,
    Right,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Indicator {
    #[default]
    Dot,
    Bar,
    Glow,
    None,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Minimize {
    /// Genie from DWM thumbnails (no pixels captured).
    Genie,
    /// The OS minimize animation into the dock icon ("Scale").
    #[default]
    Shrink,
    /// No animation for minimizes from the dock.
    None,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct DockCfg {
    pub position: Position,
    pub icon_size: f32,
    pub max_scale: f32,
    pub autohide: bool,
    /// Auto-hide: how long the dock waits after the pointer leaves before
    /// it hides (ms; read through autohide_delay, which keeps it in
    /// AUTOHIDE_DELAY_RANGE).
    pub autohide_delay_ms: u32,
    /// "On multiple displays": "follow" (one dock that follows the pointer),
    /// "main" (main display only) or "every" (a dock on every display).
    /// Empty or unknown: follow. (Older builds' `follow_mouse` switch is
    /// read into it once, on load: see parse.)
    pub displays: String,
    /// With a dock on every display: running apps show on the dock of the
    /// display their windows are on (like the taskbar's "show apps on the
    /// taskbar where the window is open"). Off by default (the taskbar's
    /// own default: every dock shows every running app).
    pub display_apps_only: bool,
    pub reserve_work_area: bool,
    pub indicator: Indicator,
    pub squircle: bool,
    pub show_trash: bool,
    /// Resting on a window preview card shows that window larger in the
    /// middle of the screen (a DWM thumbnail; nothing is moved).
    pub preview_peek: bool,
    /// What a middle click on a running app's icon does: "new_window"
    /// (start another one, like the taskbar) or "close" (ask each of its
    /// windows to close, as the window's own close button would).
    /// Anything else reads as new_window.
    pub middle_click: String,
    /// The wheel over empty dock space switches virtual desktops. Off by
    /// default (switching by accident is annoying).
    pub wheel_desktops: bool,
    pub pins: Vec<PinEntry>,
    /// app key -> icon file (relative to the app dir)
    pub icon_overrides: BTreeMap<String, String>,
    /// With a dock on every display: the docks that do not take the main
    /// dock's settings, by display key (crate::displays: the monitor's
    /// EDID maker, product and serial, else its resolution). Each entry
    /// holds only what it sets itself; anything it leaves out follows the
    /// main dock. A display with no entry is "Same as Dock 1 (main)".
    /// Empty (files from older builds): every dock is the main dock.
    pub per_display: BTreeMap<String, DisplayDock>,
    /// The display key (crate::displays) whose dock the settings above
    /// are: Dock 1, the main display's. When another display becomes the
    /// main one the dock hands over (hand_over_main), so every display
    /// keeps the dock it had. Empty: not known yet.
    pub main_display: String,
}

/// One display's own dock settings (DockCfg::per_display). A field left
/// out (None) is taken from the main dock, live.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct DisplayDock {
    /// The monitor's name when the entry was made (for people reading the
    /// file; never used to find the display).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_size: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_scale: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autohide: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pins: Option<Vec<PinEntry>>,
}

impl DisplayDock {
    /// Its own settings, starting from the main dock's as they are now
    /// ("Same as Dock 1" turned off).
    pub fn copy_of(main: &DockCfg, name: &str) -> Self {
        Self {
            name: name.to_string(),
            position: Some(main.position),
            icon_size: Some(main.icon_size),
            max_scale: Some(main.max_scale),
            autohide: Some(main.autohide),
            pins: Some(main.pins.clone()),
        }
    }

    /// Puts what it sets over `d` (the main dock's settings).
    pub fn apply(&self, d: &mut DockCfg) {
        if let Some(v) = self.position {
            d.position = v;
        }
        if let Some(v) = self.icon_size {
            d.icon_size = v;
        }
        if let Some(v) = self.max_scale {
            d.max_scale = v;
        }
        if let Some(v) = self.autohide {
            d.autohide = v;
        }
        if let Some(v) = &self.pins {
            d.pins = v.clone();
        }
    }

    /// After an edit made on the merged settings `edited` (whose main
    /// values are `main`): what it sets itself takes the edited value, and
    /// `edited` gets the main dock's values back for those fields (so the
    /// edit lands on this display only).
    fn take_back(&mut self, edited: &mut DockCfg, main: &DockCfg) {
        if self.position.is_some() {
            self.position = Some(std::mem::replace(&mut edited.position, main.position));
        }
        if self.icon_size.is_some() {
            self.icon_size = Some(std::mem::replace(&mut edited.icon_size, main.icon_size));
        }
        if self.max_scale.is_some() {
            self.max_scale = Some(std::mem::replace(&mut edited.max_scale, main.max_scale));
        }
        if self.autohide.is_some() {
            self.autohide = Some(std::mem::replace(&mut edited.autohide, main.autohide));
        }
        if self.pins.is_some() {
            self.pins = Some(std::mem::replace(&mut edited.pins, main.pins.clone()));
        }
    }
}

impl DockCfg {
    /// Index into MIDDLE_CLICKS (unknown text = new_window).
    pub fn middle_click(&self) -> usize {
        lenient(&MIDDLE_CLICKS, &self.middle_click)
    }
    /// A middle click on a running app closes its windows.
    pub fn middle_click_closes(&self) -> bool {
        self.middle_click() == 1
    }
}

impl Default for DockCfg {
    fn default() -> Self {
        Self {
            position: Position::Bottom,
            icon_size: DOCK_ICON_DEFAULT,
            // Windows-style hover tiles; magnification remains optional.
            max_scale: 1.0,
            autohide: false,
            autohide_delay_ms: AUTOHIDE_DELAY_DEFAULT,
            displays: "every".into(),
            display_apps_only: false,
            reserve_work_area: true,
            indicator: Indicator::Bar,
            squircle: false,
            show_trash: false,
            preview_peek: true,
            middle_click: "new_window".into(),
            wheel_desktops: false,
            pins: Vec::new(),
            icon_overrides: BTreeMap::new(),
            per_display: BTreeMap::new(),
            main_display: String::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct TopbarCfg {
    pub enabled: bool,
    /// DIP. 26 out of the box (the Windows top bar); files from before
    /// it that still hold the old default (32) read as 26 once (parse,
    /// height_defaults), any other height is kept.
    pub height: f32,
    /// The height format this file was written in: TOPBAR_HEIGHT_DEFAULTS
    /// (26 is the default), written by every build that has it. A file
    /// without it (0) is older: its 32 was the old default and reads as 26
    /// once (migrate_removed); from then on the marker is written, so a 32
    /// chosen again stays.
    pub height_defaults: u32,
    /// What the bar stands on (TOPBAR_STYLES): "clear" (no plate: the
    /// text and icons on the wallpaper, their color following its
    /// brightness), "frost" (one quiet frosted plate whatever the
    /// material), "recipe" (the dock's and menus' material), "solid", or
    /// "glass" (the default tint and ink that follow the wallpaper).
    /// Legacy "auto" follows the finish chosen. Transparency
    /// effects off and high contrast make it solid (topbar::look).
    pub style: String,
    /// The screen edge the bar sits on: "top" or "bottom" (every display's
    /// bar alike). Anything else reads as top; files without it are top.
    pub position: String,
    pub show_app_menus: bool,
    /// The bar's clock shows seconds (it then ticks every second while
    /// someone can see it). Off by default; files without it are off.
    pub clock_seconds: bool,
    /// Apps' tray icons on the bar (crate::tray's mirror of the
    /// notification area). Off: no tray spy at all, one button that opens
    /// Windows' own notification area instead (also a way to tell whether
    /// a problem comes from the mirror). On by default.
    pub tray: bool,
    /// Explorer's "Safely Remove Hardware" icon in the tray. Off by
    /// default: explorer ignores clicks from our tray, and the Control
    /// Center's removable storage section does the job.
    pub tray_hotplug: bool,
    /// A small dot on the Control Center button while removable storage
    /// (a USB drive, an SD card) is connected. On by default.
    pub removable_dot: bool,
    /// The main bar's own Wi-Fi, sound and battery icons, beside the
    /// Quick settings button (each opens its panel). Wi-Fi and sound off,
    /// battery (with its percent) on by default. Other displays' bars,
    /// which have no Quick settings button, show all three.
    pub show_wifi: bool,
    pub show_volume: bool,
    pub show_battery: bool,
    /// Older builds' "Status icons on every display": read (and turned
    /// into `same_on_all_displays`) but never written again.
    #[serde(skip_serializing)]
    pub tray_on_all_monitors: bool,
    /// "Same on every display": every display's bar shows everything the
    /// main bar shows (app menus, tray, desktops, input, buttons,
    /// clock). Off: the other bars show the clock and status icons.
    pub same_on_all_displays: bool,
    /// With `same_on_all_displays`: where the Live Island shows. "active"
    /// (the display of the window in front; it moves with the focus) or
    /// "every". Anything else reads as "active".
    pub island_displays: String,
    /// Tray icons the user dragged behind the chevron (left of it, shown only
    /// while the tray is expanded). Each entry is a stable tray id
    /// (`crate::tray::stable_id`): `guid:{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}`
    /// for icons that register with a GUID, else `exe:<lowercased exe
    /// path>#<uID>` (`class:<owner window class>#<uID>` if the exe path
    /// cannot be read).
    pub tray_hidden: Vec<String>,
    /// Tray icons the user dragged in front of the chevron (always shown).
    /// Same ids as `tray_hidden`. Icons in neither list are placed
    /// automatically: shown while fewer than 6 icons are shown, else hidden.
    pub tray_visible: Vec<String>,
    /// The order the user dragged the tray icons into (same ids, left to
    /// right in reading order, both sides of the chevron). Icons not in
    /// it come after, in the order they first appeared.
    pub tray_order: Vec<String>,
    /// Virtual desktop dots: "auto" (only with more than one desktop),
    /// "always" or "never". Anything else reads as "auto".
    pub show_desktops: String,
    /// The wheel over empty bar space switches virtual desktops (as over
    /// the desktop dots). Off by default: accidental switching annoys.
    pub wheel_desktops: bool,
    /// The Live Island: a pill in the middle of the primary bar showing
    /// what is happening now (topbar/island). On by default.
    pub island: bool,
    /// Its activities: taskbar progress (copies, downloads), what is
    /// playing, charger / low battery notices, camera / microphone /
    /// screen capture in use.
    pub island_progress: bool,
    pub island_media: bool,
    pub island_battery: bool,
    pub island_privacy: bool,
    /// A new screenshot on the clipboard (Win+Shift+S, Print Screen) shows
    /// in the island for a few seconds (crate::shot). Off by default: it
    /// reads the clipboard's picture (never uploaded, never logged).
    pub island_screenshot: bool,
    /// Hidden (not in Settings, written only when off): the bar keeps the
    /// space under it from maximized windows. Off only for files whose
    /// older `topbar.autohide` was on (that switch reserved nothing).
    #[serde(skip_serializing_if = "is_true")]
    pub reserve_work_area: bool,
    /// The PC status panel's "Details" are open (topbar/stats.rs). Not a
    /// row in Settings: the panel remembers how it was left. Shut by
    /// default, and written only once opened.
    #[serde(skip_serializing_if = "is_false")]
    pub stats_details: bool,
}

fn is_true(v: &bool) -> bool {
    *v
}

fn is_false(v: &bool) -> bool {
    !*v
}

/// The dock's magnification falloff radius (in icon widths) and the space
/// between its icons (DIP). Fixed: not settings.
pub const DOCK_RANGE: f32 = 2.5;
pub const DOCK_GAP: f32 = crate::theme::metrics::size::DOCK_GAP;
/// The icons' size out of the box (DIP).
pub const DOCK_ICON_DEFAULT: f32 = 44.0;

/// DockCfg::displays values, in the settings' order.
pub const DISPLAY_MODES: [&str; 3] = ["follow", "main", "every"];

/// Where the dock goes when there is more than one display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayMode {
    /// One dock, moving to the display where the pointer rests at the edge.
    Follow,
    /// One dock, on the main display.
    Main,
    /// A dock on every display.
    Every,
}

impl DisplayMode {
    pub fn index(self) -> usize {
        match self {
            DisplayMode::Follow => 0,
            DisplayMode::Main => 1,
            DisplayMode::Every => 2,
        }
    }
}

/// Auto-hide's wait before hiding (ms): the default, and what a file may
/// set (anything outside is brought inside).
pub const AUTOHIDE_DELAY_DEFAULT: u32 = 650;
pub const AUTOHIDE_DELAY_RANGE: std::ops::RangeInclusive<u32> = 200..=2000;

impl DockCfg {
    /// Auto-hide's wait before hiding, in AUTOHIDE_DELAY_RANGE (ms).
    pub fn autohide_delay(&self) -> u32 {
        self.autohide_delay_ms.clamp(*AUTOHIDE_DELAY_RANGE.start(), *AUTOHIDE_DELAY_RANGE.end())
    }

    /// The "On multiple displays" choice (an empty or unknown value is
    /// follow).
    pub fn display_mode(&self) -> DisplayMode {
        match lenient(&DISPLAY_MODES, &self.displays) {
            1 => DisplayMode::Main,
            2 => DisplayMode::Every,
            _ => DisplayMode::Follow,
        }
    }

    /// Sets the choice (index into DISPLAY_MODES).
    pub fn set_display_mode(&mut self, i: usize) {
        self.displays = DISPLAY_MODES[i.min(DISPLAY_MODES.len() - 1)].to_string();
    }

    /// Each dock shows only the running apps of its own display: a dock on
    /// every display *and* the switch on (never with one dock).
    pub fn only_display_apps(&self) -> bool {
        self.display_apps_only && self.display_mode() == DisplayMode::Every
    }

    /// The dock follows the pointer between displays.
    pub fn follows_mouse(&self) -> bool {
        self.display_mode() == DisplayMode::Follow
    }

    /// Display `key`'s own settings, if it has any (only with a dock on
    /// every display; None: it is the same as the main dock).
    pub fn own(&self, key: Option<&str>) -> Option<&DisplayDock> {
        if self.display_mode() != DisplayMode::Every {
            return None;
        }
        self.per_display.get(key?)
    }

    /// Display `new_main` is the main display now (`name`: its name;
    /// `old_name`: the old main's, if known). The settings of this
    /// section are always the main display's dock (Dock 1), so when the
    /// main display changes they hand over: the old main display gets an
    /// entry of its own with them (it keeps its dock), and the new main's
    /// own entry, if it had one, becomes these settings (it keeps its
    /// dock too). Displays "Same as Dock 1" follow the new main's. True if
    /// anything changed (the first call only remembers the key).
    pub fn hand_over_main(&mut self, new_main: &str, old_name: &str) -> bool {
        if new_main.is_empty() || self.main_display == new_main {
            return false;
        }
        let old = std::mem::replace(&mut self.main_display, new_main.to_string());
        if old.is_empty() {
            return true;
        }
        let base = self.clone();
        self.per_display.entry(old).or_insert_with(|| DisplayDock::copy_of(&base, old_name));
        if let Some(own) = self.per_display.remove(new_main) {
            own.apply(self);
        }
        true
    }

    /// The settings of display `key`'s dock: the main dock's, with what
    /// that display sets itself on top (None, or no entry: the main's).
    pub fn for_display(&self, key: Option<&str>) -> DockCfg {
        let mut d = self.clone();
        if let Some(own) = self.own(key) {
            own.apply(&mut d);
        }
        d
    }
}

impl Config {
    /// This config as display `key`'s dock sees it (its own dock settings
    /// over the main ones; everything else as it is).
    pub fn for_display(&self, key: Option<&str>) -> Config {
        if self.dock.own(key).is_none() {
            return self.clone();
        }
        let mut c = self.clone();
        c.dock = self.dock.for_display(key);
        c
    }
}

/// Applies `edit` (written for display `key`'s dock: it sees and changes
/// that dock's settings) to `c`: fields the display sets itself change in
/// its entry, everything else as usual. Without own settings (or a key)
/// the edit is the main dock's.
pub fn edit_for_display(c: &mut Config, key: Option<&str>, edit: impl FnOnce(&mut Config)) {
    let Some(mut own) = c.dock.own(key).cloned() else {
        edit(c);
        return;
    };
    let main = c.dock.clone();
    let mut e = c.for_display(key);
    edit(&mut e);
    own.take_back(&mut e.dock, &main);
    if let Some(k) = key {
        e.dock.per_display.insert(k.to_string(), own);
    }
    *c = e;
}

/// TopbarCfg::position values, in the settings' order.
pub const TOPBAR_POSITIONS: [&str; 2] = ["top", "bottom"];

/// TopbarCfg::style values, in the settings' order.
pub const TOPBAR_STYLES: [&str; 5] = ["clear", "frost", "recipe", "solid", "glass"];

/// The bar's height out of the box (DIP), and the one before the
/// Windows top bar: a file without `topbar.height_defaults` (older than
/// it) that holds exactly that is read as the new default once (parse).
pub const TOPBAR_HEIGHT: f32 = 26.0;
pub const TOPBAR_HEIGHT_OLD: f32 = 32.0;
/// TopbarCfg::height_defaults: the defaults this file was written under.
/// 2: the bar's default height is 26 (before: none, the default was 32);
/// 3: its style is "auto" (before: "clear", the default then).
pub const TOPBAR_HEIGHT_DEFAULTS: u32 = 3;
/// height_defaults from which the height is 26 by default.
const TOPBAR_SINCE_26: u64 = 2;
/// Legacy TopbarCfg::style for the finish's style (style_for).
pub const TOPBAR_STYLE_AUTO: &str = "auto";
const NATIVE_DEFAULTS_VERSION: u32 = 6;

/// TopbarCfg::island_displays values, in the settings' order.
pub const ISLAND_DISPLAYS: [&str; 2] = ["active", "every"];

/// TopbarCfg::show_desktops values, in the settings' order.
pub const DESKTOPS_MODES: [&str; 3] = ["auto", "always", "never"];
/// DockCfg::middle_click values, in the settings' order.
pub const MIDDLE_CLICKS: [&str; 2] = ["new_window", "close"];

fn lenient(list: &[&str], v: &str) -> usize {
    list.iter().position(|m| m.eq_ignore_ascii_case(v.trim())).unwrap_or(0)
}

impl TopbarCfg {
    /// Index into DESKTOPS_MODES (unknown text = auto).
    pub fn desktops_mode(&self) -> usize {
        lenient(&DESKTOPS_MODES, &self.show_desktops)
    }
    /// Index into ISLAND_DISPLAYS (unknown text = active).
    pub fn island_displays(&self) -> usize {
        lenient(&ISLAND_DISPLAYS, &self.island_displays)
    }
    /// Index into TOPBAR_STYLES (unknown text = clear).
    pub fn style(&self) -> usize {
        lenient(&TOPBAR_STYLES, &self.style)
    }
    /// The style in TOPBAR_STYLES with the finish chosen (`glass`): "auto"
    /// is clear on Glass and solid on Flat (owner,
    /// 2026-11-12); a style chosen is itself.
    pub fn style_for(&self, glass: bool) -> usize {
        if self.style.trim().eq_ignore_ascii_case(TOPBAR_STYLE_AUTO) {
            if glass {
                0
            } else {
                3
            }
        } else {
            self.style()
        }
    }
    /// Index into TOPBAR_POSITIONS (unknown text = top).
    pub fn position(&self) -> usize {
        lenient(&TOPBAR_POSITIONS, &self.position)
    }
    /// The bar sits on the bottom edge (everything anchored to it opens
    /// upward).
    pub fn at_bottom(&self) -> bool {
        self.position() == 1
    }
    /// Every display's bar shows the whole main set.
    pub fn everywhere(&self) -> bool {
        self.same_on_all_displays
    }
}

impl Default for TopbarCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            height: TOPBAR_HEIGHT,
            height_defaults: TOPBAR_HEIGHT_DEFAULTS,
            style: "glass".into(),
            position: "top".into(),
            show_app_menus: false,
            clock_seconds: false,
            tray: true,
            tray_hotplug: false,
            removable_dot: true,
            show_wifi: false,
            show_volume: false,
            show_battery: true,
            tray_on_all_monitors: false,
            same_on_all_displays: false,
            island_displays: "active".into(),
            tray_hidden: Vec::new(),
            tray_visible: Vec::new(),
            tray_order: Vec::new(),
            show_desktops: "auto".into(),
            wheel_desktops: false,
            island: true,
            island_progress: true,
            island_media: true,
            island_battery: true,
            island_privacy: true,
            island_screenshot: false,
            reserve_work_area: true,
            stats_details: false,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct AnimationCfg {
    pub speed: f32,
    pub minimize: Minimize,
    pub bounce: bool,
    pub launch: String,
}

impl Default for AnimationCfg {
    fn default() -> Self {
        Self { speed: 1.0, minimize: Minimize::Shrink, bounce: false, launch: "none".into() }
    }
}

/// Effects & performance (crate::effects; Settings > Appearance). The
/// minimize effect, launch bounce and animation speed stay in `animation`
/// (where older files have them); `preset` names the preset the items came
/// from ("custom": set one by one); `low_spec` puts everything at the
/// lightest whatever the items say.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct EffectsCfg {
    /// "low", "medium", "high", "ultra" or "custom". Anything else (older
    /// files' "auto") reads as high.
    pub preset: String,
    /// Low-spec mode (Settings > Appearance): effects and animations at
    /// their lightest (crate::effects::resolve).
    pub low_spec: bool,
    pub magnify: bool,
    pub spring: bool,
    pub label_fade: bool,
    /// Blur / acrylic / mica behind the dock and menus (off: solid).
    pub blur: bool,
    pub shadows: bool,
    pub launchpad_motion: bool,
    /// Window previews show the live window (off: name and icon only).
    pub live_previews: bool,
    /// The small animations, one switch: menus and stacks fade in, window
    /// previews pop, the Split View hint fades in, the Live Island springs
    /// open and its music bars move (off: they appear at once). Older
    /// files' separate switches are read into it (on if any was on): see
    /// parse.
    pub small_animations: bool,
    /// Dock animation frame cap: 0 = the display's rate, else 60 or 30.
    pub frame_cap: u32,
    /// "auto", "gpu" or "software" (applies when NopBar starts). Not in
    /// Settings: set in the file (or NOPBAR_RENDER=software) to measure.
    pub render: String,
}

impl Default for EffectsCfg {
    fn default() -> Self {
        Self {
            preset: "high".into(),
            low_spec: false,
            magnify: true,
            spring: true,
            label_fade: true,
            blur: true,
            shadows: true,
            launchpad_motion: true,
            live_previews: true,
            small_animations: true,
            frame_cap: 0,
            render: "auto".into(),
        }
    }
}

/// What resting the pointer in a screen corner does.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CornerAction {
    #[default]
    None,
    /// Win+Tab.
    TaskView,
    /// Win+D.
    Desktop,
    /// NopBar's Control Center on the primary top bar.
    ControlCenter,
    /// LockWorkStation.
    Lock,
    ScreenSaver,
    /// Win+N.
    Notifications,
    /// Ctrl+Win+Right / Left (nothing at the last / first desktop).
    NextDesktop,
    PreviousDesktop,
}

impl CornerAction {
    pub const ALL: [CornerAction; 9] = [
        CornerAction::None,
        CornerAction::TaskView,
        CornerAction::Desktop,
        CornerAction::ControlCenter,
        CornerAction::Lock,
        CornerAction::ScreenSaver,
        CornerAction::Notifications,
        CornerAction::NextDesktop,
        CornerAction::PreviousDesktop,
    ];

    pub fn name(self) -> &'static str {
        match self {
            CornerAction::None => "none",
            CornerAction::TaskView => "task_view",
            CornerAction::Desktop => "desktop",
            CornerAction::ControlCenter => "control_center",
            CornerAction::Lock => "lock",
            CornerAction::ScreenSaver => "screen_saver",
            CornerAction::Notifications => "notifications",
            CornerAction::NextDesktop => "next_desktop",
            CornerAction::PreviousDesktop => "previous_desktop",
        }
    }
}

/// Lenient: an action this build does not know (a newer build's, a typo)
/// reads as `none` instead of making the whole file unreadable.
impl<'de> Deserialize<'de> for CornerAction {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        let s = v.as_str().unwrap_or("");
        Ok(CornerAction::ALL.into_iter().find(|a| a.name() == s).unwrap_or_default())
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct HotCornersCfg {
    pub desktop_click: bool,
    /// The master switch (Settings > Features, Windows > Hot corners): off
    /// pauses every corner and keeps the actions chosen. On by default
    /// (the corners themselves start with no action).
    pub enabled: bool,
    pub top_left: CornerAction,
    pub top_right: CornerAction,
    pub bottom_left: CornerAction,
    pub bottom_right: CornerAction,
}

impl Default for HotCornersCfg {
    fn default() -> Self {
        Self { desktop_click: true, enabled: true, top_left: CornerAction::None, top_right: CornerAction::None, bottom_left: CornerAction::None, bottom_right: CornerAction::None }
    }
}

impl HotCornersCfg {
    /// (top_left, top_right, bottom_left, bottom_right)
    pub fn all(&self) -> [CornerAction; 4] {
        [self.top_left, self.top_right, self.bottom_left, self.bottom_right]
    }

    pub fn any(&self) -> bool {
        self.all().iter().any(|a| *a != CornerAction::None)
    }

    /// Some corner fires: the switch is on and a corner has an action.
    pub fn active(&self) -> bool {
        self.enabled && self.any()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct SearchCfg {
    /// What the top bar's search cell and the search shortcut open: "nopbar"
    /// (NopBar's own panel) or another launcher (crate::launchers ids:
    /// "powertoys_run", "cmdpal", "flow", "raycast", "everything",
    /// "windows_search"). One that is not installed opens NopBar's.
    pub with: String,
}

impl Default for SearchCfg {
    fn default() -> Self {
        Self { with: "nopbar".into() }
    }
}

/// Desktop widgets (Settings > widgets): which ones are on the desktop,
/// and whether they stay out of screen sharing and captures
/// (crate::desklayer::privacy).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct WidgetsCfg {
    pub inbox:bool,
    pub device_battery: bool,
    pub device_battery_alerts: bool,
    /// The sticky notes (off: they are closed, saved, until turned on).
    pub notes: bool,
    /// Each virtual desktop shows its own notes (crate::notes::desks);
    /// off: every note on every desktop.
    pub notes_per_desktop: bool,
    pub notes_reminders: bool,
    pub notes_lead: u8,
    /// 9, 10, or 0 (date-only reminders disabled).
    pub notes_date_hour: u8,
    /// The image pieces on the desktop (off: hidden, kept).
    pub pieces: bool,
    pub hide_from_capture: bool,
}

/// The AI agents' board (Claude Code, Codex): which are connected (their
/// hook or notify entry is NopBar's), and keeping the PC awake while one works.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct AgentsCfg {
    /// NopBar's hooks are in Claude Code's settings (Settings > Agents).
    pub claude: bool,
    /// NopBar's notify entry is in Codex's config.
    pub codex: bool,
    /// No sleep while a session works (the display may still turn off).
    pub keep_awake: bool,
}

impl Default for AgentsCfg {
    fn default() -> Self {
        Self { claude: false, codex: false, keep_awake: true }
    }
}

impl Default for WidgetsCfg {
    fn default() -> Self {
        Self { inbox:true, device_battery: false, device_battery_alerts: true, notes: true, notes_per_desktop: true, notes_reminders: true, notes_lead: 10, notes_date_hour: 9, pieces: true, hide_from_capture: false }
    }
}

/// The focus timer (crate::focus_timer).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct FocusTimerCfg {
    /// The custom plan's focus and break, in minutes (25 / 5 and 50 / 10
    /// are always offered).
    pub custom_focus: u32,
    pub custom_break: u32,
    /// Turn on Windows' Focus (Do not disturb) while a focus phase runs.
    pub dnd: bool,
    /// A gentle chime when a phase ends.
    pub chime: bool,
    /// A notification too when a phase ends (it replaces the chime).
    pub toast: bool,
    /// "Focus timer" in the clock menu (Settings > Features). Off hides it
    /// while no timer runs; a running one is always shown. On by default.
    pub enabled: bool,
}

impl Default for FocusTimerCfg {
    fn default() -> Self {
        Self { custom_focus: 40, custom_break: 8, dnd: true, chime: true, toast: false, enabled: true }
    }
}

/// One thing on a Launchpad page: an app (its shell:AppsFolder parsing
/// name, `App::id`) or a folder of apps. Written as a plain string or as
/// `{"folder": name, "apps": [ids]}` (`"auto": true` added for a folder
/// "Tidy Home" made).
#[derive(Clone, Debug)]
pub enum LpItem {
    App(String),
    /// `id`: who the folder is while Launchpad is open (screen readers keep
    /// hold of it through moves and renames). Never saved; 0 = none given
    /// yet (launchpad::layout_store::arrange gives one). Not compared.
    /// `auto`: "Tidy Home" made it (launchpad::tidy), so tidying again may
    /// sort its apps anew; saved. Off for every folder the user made or
    /// changed since (renamed, an app put in or taken out), and for every
    /// folder an older file has.
    Folder { name: String, apps: Vec<String>, id: u64, auto: bool },
}

impl PartialEq for LpItem {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (LpItem::App(a), LpItem::App(b)) => a == b,
            (LpItem::Folder { name: n1, apps: a1, auto: x1, .. }, LpItem::Folder { name: n2, apps: a2, auto: x2, .. }) => n1 == n2 && a1 == a2 && x1 == x2,
            _ => false,
        }
    }
}

impl Eq for LpItem {}

impl Serialize for LpItem {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            LpItem::App(id) => s.serialize_str(id),
            LpItem::Folder { name, apps, auto: false, .. } => serde_json::json!({"folder": name, "apps": apps}).serialize(s),
            LpItem::Folder { name, apps, auto: true, .. } => serde_json::json!({"folder": name, "apps": apps, "auto": true}).serialize(s),
        }
    }
}

impl LpItem {
    /// A folder the user made (no session id yet).
    #[cfg(test)]
    pub fn folder(name: impl Into<String>, apps: Vec<String>) -> LpItem {
        LpItem::Folder { name: name.into(), apps, id: 0, auto: false }
    }

    /// Lenient: anything this build cannot read becomes an app with no id
    /// (never installed, so it drops out: launchpad::layout_store::arrange).
    pub fn from_value(v: &serde_json::Value) -> LpItem {
        match v {
            serde_json::Value::String(id) => LpItem::App(id.clone()),
            serde_json::Value::Object(o) => match o.get("apps").and_then(|a| a.as_array()) {
                Some(apps) => LpItem::Folder {
                    name: o.get("folder").and_then(|n| n.as_str()).unwrap_or("").to_string(),
                    apps: apps.iter().filter_map(|a| a.as_str().map(String::from)).collect(),
                    id: 0,
                    // (Only a plain true: anything else, or an older file, is the user's.)
                    auto: o.get("auto").and_then(|a| a.as_bool()).unwrap_or(false),
                },
                None => LpItem::App(String::new()),
            },
            _ => LpItem::App(String::new()),
        }
    }
}

/// The `launchpad.schema` that makes `layout` the Home pages exactly (the
/// App Library era): apps installed since are not added to them.
pub const LP_LIBRARY_SCHEMA: u32 = 1;

/// Launchpad's own arrangement (launchpad/layout_store.rs) and its App
/// Library (launchpad/library.rs). Everything default is not written.
/// Older files kept the hotkey and the Windows key here
/// (`launchpad.hotkey`, `launchpad.win_key`, read into `shortcuts` on
/// load): those are ignored.
///
/// Schema 0 (older files): `layout` was merged with every app installed
/// (new ones appended). The first open of this version with the whole app
/// list read turns that, as shown, into the Home pages and sets schema 1:
/// from then on new apps go to the Library only (layout_store::migrate).
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct LaunchpadCfg {
    /// Home: pages of items, in order (set by arranging Launchpad and its
    /// menus; Settings' "Reset to alphabetical" empties it and the schema).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub layout: Vec<Vec<LpItem>>,
    /// 0: an older file (see above); LP_LIBRARY_SCHEMA: `layout` is Home.
    #[serde(skip_serializing_if = "is_zero")]
    pub schema: u32,
    /// The App Library page (Settings > General). Off: no Library page and
    /// new apps go onto the pages as before; hidden apps stay hidden.
    #[serde(skip_serializing_if = "is_true")]
    pub show_library: bool,
    /// Apps Launchpad has seen (since the migration): an app installed and
    /// not listed here is new (a dot) until it is opened or moved.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub seen: Vec<String>,
    /// The user's category for an app (app id -> library::Cat key), over
    /// the heuristics.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub library: std::collections::BTreeMap<String, String>,
    /// Apps the user hid: off Home, in the Library's Hidden category, still
    /// found by search (dimmed).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hidden: Vec<String>,
    /// The clean-up suggestions banner was dismissed (or used).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub cleanup_dismissed: bool,
}

fn is_zero(v: &u32) -> bool {
    *v == 0
}

impl Default for LaunchpadCfg {
    fn default() -> Self {
        LaunchpadCfg {
            layout: Vec::new(),
            schema: 0,
            show_library: true,
            seen: Vec::new(),
            library: Default::default(),
            hidden: Vec::new(),
            cleanup_dismissed: false,
        }
    }
}

impl LaunchpadCfg {
    pub fn is_default(&self) -> bool {
        *self == LaunchpadCfg::default()
    }
}

/// Lenient: `launchpad` that is not an object, a field of the wrong type,
/// a page that is not a list or an odd entry in a list never makes the
/// whole file unreadable (they read as empty / their default).
impl<'de> Deserialize<'de> for LaunchpadCfg {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        let pages = v.get("layout").and_then(|l| l.as_array()).map(|ps| ps.iter().map(|p| p.as_array().map(|items| items.iter().map(LpItem::from_value).collect()).unwrap_or_default()).collect()).unwrap_or_default();
        let strings = |k: &str| -> Vec<String> { v.get(k).and_then(|l| l.as_array()).map(|a| a.iter().filter_map(|s| s.as_str()).filter(|s| !s.is_empty()).map(String::from).collect()).unwrap_or_default() };
        let library = v
            .get("library")
            .and_then(|m| m.as_object())
            .map(|m| m.iter().filter_map(|(k, c)| c.as_str().filter(|_| !k.is_empty()).map(|c| (k.clone(), c.to_string()))).collect())
            .unwrap_or_default();
        let d = LaunchpadCfg::default();
        Ok(LaunchpadCfg {
            layout: pages,
            schema: v.get("schema").and_then(|s| s.as_u64()).map_or(d.schema, |s| s.min(u32::MAX as u64) as u32),
            show_library: v.get("show_library").and_then(|b| b.as_bool()).unwrap_or(d.show_library),
            seen: strings("seen"),
            library,
            hidden: strings("hidden"),
            cleanup_dismissed: v.get("cleanup_dismissed").and_then(|b| b.as_bool()).unwrap_or(d.cleanup_dismissed),
        })
    }
}

/// One per-app input rule (crate::ime, Settings > Input).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct InputRule {
    /// Which app: "aumid:<AUMID>", "exe:<lowercase exe path>" or
    /// "name:<exe file name>" (any exe of that name).
    pub app: String,
    /// What the settings list calls it.
    pub name: String,
    /// "native" (Hangul, kana, Chinese), "alpha" (English), "remember" or
    /// "ignore" (never touched). Anything else reads as ignore.
    pub mode: String,
}

/// The input indicator and per-app input memory (crate::ime).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct InputCfg {
    /// The top bar's input cell: "auto" (with an IME or more than one
    /// keyboard layout installed), "always" or "never".
    pub indicator: String,
    /// Remember each app's 한/영 (native / Latin) mode when it is left and
    /// put it back when it returns. Off by default: opt-in.
    pub remember: bool,
    /// Per-app rules, first match wins; they beat the memory.
    pub rules: Vec<InputRule>,
}

impl Default for InputCfg {
    fn default() -> Self {
        Self { indicator: "auto".into(), remember: false, rules: Vec::new() }
    }
}

/// InputCfg::indicator values, in the settings' order.
pub const INDICATOR_MODES: [&str; 3] = ["auto", "always", "never"];

impl InputCfg {
    /// Index into INDICATOR_MODES (unknown text = auto).
    pub fn indicator_mode(&self) -> usize {
        lenient(&INDICATOR_MODES, &self.indicator)
    }
}

/// One calendar subscription (an iCalendar link: Google's "secret address
/// in iCal format", Outlook.com's published ICS link, any webcal:// or
/// https:// .ics). The link is a secret: it is never logged in full and
/// never sent anywhere but its own host (crate::agenda).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct CalendarFeed {
    /// What Settings calls it (the link's host until renamed).
    pub name: String,
    pub url: String,
}

/// Today's agenda under the clock panel's month (crate::agenda).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct CalendarCfg {
    /// Show the agenda. On by default, but nothing shows until a source
    /// is set (then one dim row offers to add a calendar).
    pub agenda: bool,
    /// Tomorrow's events too, under a small heading, when today has few
    /// left. On by default.
    pub tomorrow: bool,
    /// Windows' own calendar store (the calendars other apps keep in
    /// Windows). Off by default: few apps write to it any more.
    pub windows: bool,
    /// The subscriptions, in the order shown.
    pub feeds: Vec<CalendarFeed>,
}

impl Default for CalendarCfg {
    fn default() -> Self {
        Self { agenda: true, tomorrow: true, windows: false, feeds: Vec::new() }
    }
}

impl CalendarCfg {
    /// A source is set: a feed, or the Windows store.
    pub fn has_source(&self) -> bool {
        self.windows || self.feeds.iter().any(|f| !f.url.trim().is_empty())
    }
}

/// Update checks (the direct-download build only; a Store build is
/// updated by the Store). See crate::updates.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct UpdatesCfg {
    /// Look for a new version 30 s after startup and then once a day.
    pub auto_check: bool,
    /// A version found is downloaded and installed at a quiet moment
    /// (crate::autoupdate). Off: "Ask first" (a toast offers Install now).
    pub auto_install: bool,
    /// Testing only (not in Settings): another https manifest to check,
    /// e.g. a gist with a higher version, to try the toast on a PC before
    /// nopbard.com is up. Unset = the official manifest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_url: Option<String>,
}

impl Default for UpdatesCfg {
    fn default() -> Self {
        Self { auto_check: true, auto_install: true, manifest_url: None }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct ThemeRef {
    /// "builtin:<name>" or "file:themes\\<name>.json"
    #[serde(rename = "ref")]
    pub source: String,
    /// Light or dark over the theme's own choice: "" (the theme's: Graphite
    /// follows Windows), "system", "light" or "dark".
    pub mode: String,
    /// The accent color over the theme's: "" (Graphite's neutral default),
    /// "system" (Windows' on light, none on dark), or
    /// "#RRGGBB" (a preset or one's own, in both modes).
    pub accent: String,
    /// How surfaces are finished (crate::theme::Finish): "flat" or "glass";
    /// "" (never chosen) is the default, Glass; anything unknown is Flat.
    /// The glass v3 trio's "frost" and "clear" (0.1.63) read as Glass
    /// (OLD_GLASSES, settle_removed: one glass since 2026-11-11).
    pub finish: String,
}

impl Default for ThemeRef {
    fn default() -> Self {
        Self { source: "builtin:graphite".into(), mode: String::new(), accent: String::new(), finish: FINISHES[FINISH_DEFAULT].into() }
    }
}

/// The Look page's light / dark choice (ThemeRef::mode), by option index.
pub const THEME_MODES: [&str; 3] = ["system", "light", "dark"];

/// The Look page's finish choice (ThemeRef::finish), by option index:
/// Flat, then Glass (glass v3's B: the owner's two, 2026-11-11).
pub const FINISHES: [&str; 2] = ["flat", "glass"];
/// The finish a new install has, and a file that never chose: Glass.
pub const FINISH_DEFAULT: usize = 1;
/// The glass v3 trio's other two (0.1.63: Frost A and Clear C): read as
/// Glass, and written back as "glass" (Config::settle_removed).
pub const OLD_GLASSES: [&str; 2] = ["frost", "clear"];

/// The Look page's accent colors (ThemeRef::accent): Windows' own, then a
/// few fixed ones (text keys of their names).
pub const ACCENTS: [(&str, &str); 9] = [
    ("system", "settings.theme.accent.system"),
    ("#0A84FF", "settings.theme.accent.blue"),
    ("#BF5AF2", "settings.theme.accent.purple"),
    ("#FF375F", "settings.theme.accent.pink"),
    ("#FF453A", "settings.theme.accent.red"),
    ("#FF9F0A", "settings.theme.accent.orange"),
    ("#FFD60A", "settings.theme.accent.yellow"),
    ("#30D158", "settings.theme.accent.green"),
    ("#8E8E93", "settings.theme.accent.graphite"),
];

/// The accent row's last option, after ACCENTS: a color of one's own
/// (Windows' color dialog), its text key.
pub const ACCENT_CUSTOM: usize = ACCENTS.len();
pub const ACCENT_CUSTOM_TEXT: &str = "settings.theme.accent.custom";
/// "Custom…" picked without a dialog (a test, an old file): a neutral grey
/// off the list.
const CUSTOM_DEFAULT: &str = "#808080";

/// `c` as the config spells an accent: "#RRGGBB".
pub fn accent_hex(c: crate::theme::Rgba) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b)
}

impl ThemeRef {
    /// The light / dark option shown (an empty or unknown mode: Automatic).
    pub fn mode_index(&self) -> usize {
        THEME_MODES.iter().position(|m| m.eq_ignore_ascii_case(self.mode.trim())).unwrap_or(0)
    }

    /// The accent option shown: its place on the list, "Custom…"
    /// (ACCENT_CUSTOM) for any other color, Automatic for "" and the rest.
    pub fn accent_index(&self) -> usize {
        let a = self.accent.trim();
        match ACCENTS.iter().position(|(c, _)| c.eq_ignore_ascii_case(a)) {
            Some(i) => i,
            None if crate::theme::parse_color(a).is_some() => ACCENT_CUSTOM,
            None => 0,
        }
    }

    /// The accent row's choice `i` applied: a color on the list, or for
    /// "Custom…" the color already chosen (the settings window asks for a
    /// new one first; anything else keeps CUSTOM_DEFAULT).
    pub fn set_accent_index(&mut self, i: usize) {
        if i < ACCENTS.len() {
            self.accent = ACCENTS[i].0.to_string();
        } else if self.accent_index() != ACCENT_CUSTOM {
            self.accent = CUSTOM_DEFAULT.to_string();
        }
    }

    /// The custom accent as `#RRGGBB` (None unless one is chosen).
    pub fn custom_accent(&self) -> Option<crate::theme::Rgba> {
        (self.accent_index() == ACCENT_CUSTOM).then(|| crate::theme::parse_color(self.accent.trim())).flatten()
    }

    /// The finish option shown (empty: the default, Glass; the old Frost
    /// and Clear: Glass; unknown: Flat).
    pub fn finish_index(&self) -> usize {
        let f = self.finish.trim();
        if f.is_empty() || OLD_GLASSES.iter().any(|o| o.eq_ignore_ascii_case(f)) {
            return FINISH_DEFAULT;
        }
        FINISHES.iter().position(|n| n.eq_ignore_ascii_case(f)).unwrap_or(0)
    }

    /// The theme `source` names, loaded, with this choice of light / dark
    /// and accent over its own (crate::theme::Theme::load).
    pub fn load(&self, app_dir: &std::path::Path) -> crate::theme::Theme {
        let mut t = crate::theme::Theme::load(&self.source, app_dir);
        let mode = self.mode.trim();
        if THEME_MODES.contains(&mode) {
            t.mode = mode.to_string();
        }
        let accent = self.accent.trim();
        if accent == "system" || crate::theme::parse_color(accent).is_some() {
            t.accent = accent.to_string();
        }
        t.finish = crate::theme::Finish::from_name(FINISHES[self.finish_index()]);
        t
    }
}

/// Settings > Appearance's text: the UI font and the text size, applied
/// process-wide (crate::fonts). Read leniently: a value of the wrong type
/// is the default, never a broken file.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct AppearanceCfg {
    /// A font family by its English name ("" = the built-in Pretendard,
    /// "system" = Windows' UI font; crate::fonts::pick).
    #[serde(deserialize_with = "any_string")]
    pub font: String,
    /// "small", "default" or "large" (anything else reads as default).
    #[serde(deserialize_with = "any_string")]
    pub text_size: String,
    /// The font format this file was written in: FONT_DEFAULTS (the
    /// built-in Pretendard is the default, "system" an explicit choice of
    /// Windows' font), written by every build that has it. A file without
    /// it (0) is older: its "" was Windows' UI font as the default, and it
    /// becomes the new default (migrate_font).
    pub font_defaults: u32,
}

impl Default for AppearanceCfg {
    fn default() -> Self {
        Self { font: String::new(), text_size: String::new(), font_defaults: FONT_DEFAULTS }
    }
}

/// AppearanceCfg::font_defaults since the built-in Pretendard is the
/// default font (before: none, "" was Windows' UI font).
pub const FONT_DEFAULTS: u32 = 2;

impl AppearanceCfg {
    /// The text size option shown (crate::fonts::TEXT_SIZES).
    pub fn text_size_index(&self) -> usize {
        crate::fonts::size_index(&self.text_size)
    }
}

/// A string, or "" for any other JSON value.
fn any_string<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(v.as_str().map(|s| s.trim().to_string()).unwrap_or_default())
}

/// The appearance block, or its default when it is not an object.
fn any_appearance<'de, D: serde::Deserializer<'de>>(d: D) -> Result<AppearanceCfg, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(v).unwrap_or_default())
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    /// One-time Windows-native defaults migrations, independent of the file schema.
    pub native_defaults_version: u32,
    pub dock: DockCfg,
    pub topbar: TopbarCfg,
    pub animation: AnimationCfg,
    pub search: SearchCfg,
    /// Desktop widgets (notes, image pieces).
    pub widgets: WidgetsCfg,
    /// The AI agents' board.
    pub agents: AgentsCfg,
    pub companion: crate::companion::model::Config,
    pub chat: crate::companion::chat::state::Config,
    /// The focus timer.
    pub focus_timer: FocusTimerCfg,
    pub hot_corners: HotCornersCfg,
    /// The input indicator and per-app input memory.
    pub input: InputCfg,
    /// Today's agenda in the clock panel (crate::agenda).
    pub calendar: CalendarCfg,
    /// Launchpad's pages and folders (empty: name order, not written).
    #[serde(skip_serializing_if = "LaunchpadCfg::is_default")]
    pub launchpad: LaunchpadCfg,
    /// Every shortcut by registry id (crate::shortcuts): "alt+space",
    /// "none", ... Missing ids are read from an older file's keys
    /// (Shortcut::legacy, on load), else the default.
    #[serde(default)]
    pub shortcuts: BTreeMap<String, String>,
    pub effects: EffectsCfg,
    pub theme: ThemeRef,
    /// The UI font and text size.
    #[serde(deserialize_with = "any_appearance")]
    pub appearance: AppearanceCfg,
    pub updates: UpdatesCfg,
    pub hide_taskbar: bool,
    pub start_with_windows: bool,
    /// NopBar's own icon in the notification area (dock/trayicon.rs).
    pub tray_icon: bool,
    /// UI language: "auto" (follow Windows) or a catalog tag ("en", "ko").
    pub language: String,
    /// The old first-run Welcome page was opened (kept so an older build
    /// does not show its own again; the 3-step flow goes by welcome_done).
    pub welcomed: bool,
    /// The first-run flow ended with Done or Skip: it is not shown again.
    /// A file without the field predates the flow: see welcome_done_in.
    pub welcome_done: bool,
    /// The settings window's last size (client area, DIP), not shown as a
    /// row: it opens at it again. Not written until it was resized.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings_window: Option<[u32; 2]>,
    /// Never written to disk. Set when this process must not overwrite the
    /// file: it is from a newer version, or it could not be read / backed up.
    #[serde(skip)]
    pub read_only: bool,
}

impl Default for Config {
    fn default() -> Self {
        let mut c = Self {
            version: VERSION,
            native_defaults_version: NATIVE_DEFAULTS_VERSION,
            dock: DockCfg::default(),
            topbar: TopbarCfg::default(),
            animation: AnimationCfg::default(),
            search: SearchCfg::default(),
            widgets: WidgetsCfg::default(),
            agents: AgentsCfg::default(),
            companion: crate::companion::model::Config::default(),
            chat: crate::companion::chat::state::Config::default(),
            focus_timer: FocusTimerCfg::default(),
            hot_corners: HotCornersCfg::default(),
            input: InputCfg::default(),
            calendar: CalendarCfg::default(),
            launchpad: LaunchpadCfg::default(),
            shortcuts: BTreeMap::new(),
            effects: EffectsCfg::default(),
            theme: ThemeRef::default(),
            appearance: AppearanceCfg::default(),
            updates: UpdatesCfg::default(),
            hide_taskbar: true,
            start_with_windows: true,
            tray_icon: true,
            language: "auto".into(),
            welcomed: false,
            settings_window: None,
            welcome_done: false,
            read_only: false,
        };
        c.migrate_shortcuts(None);
        c
    }
}

/// What reading a file does with a field whose Settings row was removed
/// (the declutter). Never turns on what a user turned off.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Settle {
    /// Back to the default (values that turn nothing on or off: a wait, a
    /// choice of place; or a switch that is off by default).
    Default,
    /// A switch on by default: stays off if it was turned off.
    OffStays,
    /// Kept as it is (a limit the user set: lowering effects).
    Kept,
    /// Its own rule (tested apart): hot corners' master switch.
    Own,
}

/// Fields whose Settings row was removed: a file may still hold them (read
/// without error); settle_removed applies the rule on every read. Paths as
/// in the file ("section.field"); checked by the tests.
#[cfg(test)]
pub const DEFAULTED: &[(&str, Settle)] = &[
    ("effects.shadows", Settle::OffStays),
    ("effects.live_previews", Settle::OffStays),
    ("effects.frame_cap", Settle::Kept),
    // Auto and its switch went (Low-spec mode is the one row now): the
    // preset the items came from stays as set.
    ("effects.preset", Settle::Kept),
    // The animation items "Reduce motion" set together: no row of their
    // own now (Low-spec mode only switches; resolve does the rest). Kept
    // as chosen.
    ("effects.small_animations", Settle::Kept),
    ("effects.spring", Settle::Kept),
    ("effects.label_fade", Settle::Kept),
    ("effects.launchpad_motion", Settle::Kept),
    ("animation.bounce", Settle::Kept),
    ("animation.minimize", Settle::Kept),
    ("animation.speed", Settle::Default),
    ("dock.autohide_delay_ms", Settle::Default),
    ("dock.preview_peek", Settle::OffStays),
    ("dock.middle_click", Settle::Default),
    ("topbar.island_displays", Settle::Default),
    ("topbar.island_progress", Settle::OffStays),
    ("topbar.island_media", Settle::OffStays),
    ("topbar.island_battery", Settle::OffStays),
    ("topbar.island_privacy", Settle::OffStays),
    ("calendar.windows", Settle::Default),
    ("calendar.tomorrow", Settle::OffStays),
    ("hot_corners.enabled", Settle::Own),
    ("focus_timer.toast", Settle::Default),
    // The settings structure pass (docs/reviews/2026-11-08-settings-
    // structure-audit.md): Effects' magnification is the Dock's "Off"
    // (moved over once: settle_removed), the safely-remove extras and the
    // agenda switch (the calendar links are the switch).
    ("effects.magnify", Settle::Default),
    ("topbar.tray_hotplug", Settle::Kept),
    ("topbar.removable_dot", Settle::OffStays),
    ("calendar.agenda", Settle::Default),
];

/// The Dock's magnification "Off" (settings::model::MAGNIFY_STEPS[0]).
pub const MAGNIFY_OFF: f32 = 1.0;

/// The surface finish choices (Settings > Appearance > Finish): Flat
/// (opaque, no blur) and Glass (the default; only it blurs:
/// effects::items_of). The old Solid and the old translucent Flat read as
/// Flat; the old Glass, Frost and Clear as Glass (settle_removed).
/// One control for `theme.finish` and `effects.blur`.
pub const FINISH_CHOICES: usize = FINISHES.len();

impl Config {
    /// Low-spec mode (Settings > Appearance): only the switch. What is in
    /// force comes from crate::effects::resolve (on: everything at its
    /// lightest); the stored items are never touched, so off gives back
    /// exactly what was chosen.
    pub fn set_low_spec(&mut self, on: bool) {
        self.effects.low_spec = on;
    }

    /// Existing hook and bubble switches become one; old keys stay readable.
    pub fn unify_agent_notifications(&mut self) {
        self.set_agent_notifications(0, self.agents.claude || self.chat.claude_mcp);
        self.set_agent_notifications(1, self.agents.codex || self.chat.codex_mcp);
    }

    pub fn set_agent_notifications(&mut self, provider: u8, on: bool) {
        match provider {
            0 => { self.agents.claude = on; self.chat.claude_mcp = on; }
            1 => { self.agents.codex = on; self.chat.codex_mcp = on; }
            _ => self.chat.antigravity_mcp = on,
        }
    }

    /// Every animation item off, as the old "Reduce motion" left them
    /// (its signature: config::migrate_removed). Pure: tested.
    pub fn motion_all_off(&self) -> bool {
        let e = &self.effects;
        !e.small_animations && !e.spring && !e.label_fade && !e.launchpad_motion && !self.animation.bounce && self.animation.minimize == Minimize::None
    }

    /// The animation items as a new install has them (once, when "Reduce
    /// motion" becomes Low-spec mode: the mode does that job now).
    fn motion_defaults(&mut self) {
        let (e, a) = (EffectsCfg::default(), AnimationCfg::default());
        self.effects.small_animations = e.small_animations;
        self.effects.spring = e.spring;
        self.effects.label_fade = e.label_fade;
        self.effects.launchpad_motion = e.launchpad_motion;
        self.animation.bounce = a.bounce;
        self.animation.minimize = a.minimize;
    }

    /// An older build's "Reduce motion" (read from the small animations'
    /// switch) as Low-spec mode, once: whoever turned it on wanted lighter.
    /// When it had turned every animation item off (all six, its own
    /// mark), they come back as a new install has them, since the mode
    /// does that job now and no switch is left to turn them on one by
    /// one; a file with only some of them off keeps them as they are.
    pub fn low_spec_from_reduce_motion(&mut self) {
        self.effects.low_spec = !self.effects.small_animations;
        if self.motion_all_off() {
            self.motion_defaults();
        }
    }

    /// The finish shown (0 Flat, 1 Glass): FINISHES' order.
    pub fn finish_choice(&self) -> usize {
        self.theme.finish_index()
    }

    /// Flat (opaque: the blur is not used, whatever `effects.blur` says)
    /// or a glass (the blur on: a glass is the blur; Low-spec mode and
    /// high contrast still turn it off: crate::effects).
    pub fn set_finish_choice(&mut self, i: usize) {
        let i = i.min(FINISHES.len() - 1);
        self.theme.finish = FINISHES[i].to_string();
        if i > 0 {
            self.effects.blur = true;
        }
    }

    /// The finish is Glass (it blurs).
    pub fn glass(&self) -> bool {
        self.theme.finish_index() > 0
    }

    /// Fields without a row now (DEFAULTED) and choices that became
    /// switches, as read from a file. The rule: nothing a user turned off is
    /// turned on. A removed switch keeps "off" (or goes to its default when
    /// that is off); other removed values go back to the default; switches
    /// made one are off if any part was off. The effects' preset and motion
    /// items are left as set (Settings' switches read them).
    pub fn settle_removed(&mut self) {
        let d = Config::default();
        // Removed switches: off stays off, on becomes the default.
        macro_rules! off_stays {
            ($($f:ident).+) => {
                self.$($f).+ = self.$($f).+ && d.$($f).+;
            };
        }
        off_stays!(effects.shadows);
        off_stays!(effects.live_previews);
        off_stays!(dock.preview_peek);
        off_stays!(topbar.island_progress);
        off_stays!(topbar.island_media);
        off_stays!(topbar.island_battery);
        off_stays!(topbar.island_privacy);
        off_stays!(calendar.windows);
        off_stays!(calendar.tomorrow);
        off_stays!(focus_timer.toast);
        off_stays!(topbar.removable_dot);
        // The finish is Flat or Glass: a glass without the blur drew
        // solid (shown as Solid), so it reads as Flat. The blur switch
        // itself is left as it was (Flat never uses it). The glass v3
        // trio's Frost and Clear are Glass now (2026-11-11), written so.
        if self.glass() && !self.effects.blur {
            self.theme.finish = FINISHES[0].to_string();
        } else if OLD_GLASSES.iter().any(|o| o.eq_ignore_ascii_case(self.theme.finish.trim())) {
            self.theme.finish = FINISHES[FINISH_DEFAULT].to_string();
        }
        // Magnification has one switch, the Dock's (Off is its first
        // step): whoever turned Effects' magnification off gets every
        // dock's set to Off, once (the switch is on from then on).
        if !self.effects.magnify {
            self.dock.max_scale = MAGNIFY_OFF;
            for d in self.dock.per_display.values_mut() {
                if d.max_scale.is_some() {
                    d.max_scale = Some(MAGNIFY_OFF);
                }
            }
            self.effects.magnify = true;
        }
        // Removed values that turn nothing on or off.
        self.animation.speed = d.animation.speed;
        self.dock.autohide_delay_ms = d.dock.autohide_delay_ms;
        self.dock.middle_click = d.dock.middle_click.clone();
        self.topbar.island_displays = d.topbar.island_displays.clone();
        self.calendar.agenda = d.calendar.agenda;
        // Indicator styles are user choices; never collapse bars to dots.
        // "Always" is Auto now (on or off only): shows less, never more.
        if self.topbar.desktops_mode() == 1 {
            self.topbar.show_desktops = DESKTOPS_MODES[0].into();
        }
        if self.input.indicator_mode() == 1 {
            self.input.indicator = INDICATOR_MODES[0].into();
        }
        // Scrolling to switch desktops: one switch, on only where both were.
        let wheel = self.dock.wheel_desktops && self.topbar.wheel_desktops;
        (self.dock.wheel_desktops, self.topbar.wheel_desktops) = (wheel, wheel);
        // Automatic updates: one switch, on only if checking and
        // installing both were (a missing field is its default, on).
        let auto = self.updates.auto_check && self.updates.auto_install;
        (self.updates.auto_check, self.updates.auto_install) = (auto, auto);
        // Hot corners lost their master switch: corners paused by it lose
        // their actions instead (none acts, as before).
        if !self.hot_corners.enabled {
            self.hot_corners = HotCornersCfg { desktop_click: self.hot_corners.desktop_click, ..Default::default() };
        }
    }

    /// The theme to draw with (ThemeRef::load), after making this file's
    /// font and text size current (crate::fonts is process-wide), so the
    /// theme resolves with them.
    pub fn load_theme(&self, app_dir: &std::path::Path) -> crate::theme::Theme {
        crate::fonts::apply(&self.appearance.font, &self.appearance.text_size);
        self.theme.load(app_dir)
    }

    /// Shortcut `id`'s value (crate::shortcuts): the map's, else the
    /// registry default ("none" for an unknown id).
    pub fn shortcut(&self, id: &str) -> String {
        if let Some(v) = self.shortcuts.get(id) {
            return v.clone();
        }
        crate::shortcuts::find(id).map(|s| s.default.to_string()).unwrap_or_else(|| "none".into())
    }

    /// Sets shortcut `id`.
    pub fn set_shortcut(&mut self, id: &str, v: &str) {
        let v = v.trim().to_ascii_lowercase();
        let v = if crate::shortcuts::is_off(&v) { "none".to_string() } else { v };
        self.shortcuts.insert(id.to_string(), v);
    }

    /// Fills the map for every id it lacks from the older keys of the file
    /// `old` (as read: a file from before the registry, or one an older
    /// build rewrote without the map), else the default, and drops values
    /// that are not valid for their shortcut.
    pub fn migrate_shortcuts(&mut self, old: Option<&serde_json::Value>) {
        for s in crate::shortcuts::ALL {
            if s.kind == crate::shortcuts::Kind::Fixed {
                self.shortcuts.remove(s.id);
                continue;
            }
            let have = self.shortcuts.get(s.id).filter(|v| crate::shortcuts::valid(s, v)).cloned();
            let old = old.and_then(|o| crate::shortcuts::legacy_value(o, s)).filter(|v| crate::shortcuts::valid(s, v));
            let v = have.or(old).unwrap_or_else(|| {
                if s.id == crate::shortcuts::DOCK_APPS && !self.hide_taskbar { "none".into() } else { s.default.to_string() }
            });
            self.set_shortcut(s.id, &v);
        }
    }
}

pub fn app_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("NopBar")
}

pub fn file() -> PathBuf {
    app_dir().join("config.json")
}

/// The file an icon override names: only "icons\<file>" inside the app
/// folder counts (no other folders, no "..", no absolute or network paths).
pub fn icon_file(rel: &str) -> Option<String> {
    let name = rel.strip_prefix("icons\\").or_else(|| rel.strip_prefix("icons/"))?;
    if name.is_empty() || name.contains(['\\', '/', ':']) || name.starts_with('.') {
        return None;
    }
    let p = app_dir().join("icons").join(name);
    p.is_file().then(|| p.to_string_lossy().into_owned())
}

pub fn text_hash(text: &str) -> u64 {
    crate::icons::fnv64(text.as_bytes())
}

/// Bumped on every save. Watcher events carry the value seen when they read
/// the file; an event older than a later save of ours is stale.
static WRITE_SEQ: AtomicU64 = AtomicU64::new(0);
/// Hash of the config text loaded at startup (the watcher's baseline).
static LOADED: AtomicU64 = AtomicU64::new(0);

pub fn write_seq() -> u64 {
    WRITE_SEQ.load(Ordering::SeqCst)
}

pub fn loaded_hash() -> u64 {
    LOADED.load(Ordering::SeqCst)
}

/// True if `text` is exactly what this process last saved.
pub fn is_own_write(text: &str) -> bool {
    LAST_WRITTEN.load(Ordering::SeqCst) == text_hash(text)
}

fn yes() -> bool {
    true
}

/// The v1 file, typed strictly: wrong types are an error, not a default.
#[derive(Deserialize)]
struct V1 {
    pins: Vec<PinEntry>,
    #[serde(default = "yes")]
    hide_taskbar: bool,
}

pub struct Parsed {
    pub cfg: Config,
    /// The schema or native defaults changed and should be saved once.
    pub migrated: bool,
}

/// Parses a config file. The top level must be an object; v1 (no
/// "version", or version 1) is converted; newer versions are read as far as
/// we understand them and kept read-only so we never downgrade them.
/// Whether a config file's first-run flow is over. Every file this build
/// writes says so ("welcome_done"); a file without it was written by an
/// older NopBar, so its user has been here before (the old Welcome page
/// marked itself "welcomed", and files from before that had no first-run
/// page at all): done, never shown to them.
pub fn welcome_done_in(obj: &serde_json::Map<String, serde_json::Value>) -> bool {
    obj.get("welcome_done").and_then(|v| v.as_bool()).unwrap_or(true)
}

pub fn parse(text: &str) -> Result<Parsed, String> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let Some(obj) = v.as_object() else {
        return Err("top level is not a JSON object".into());
    };
    let welcome_done = welcome_done_in(obj);
    let version = match obj.get("version") {
        None => 1,
        Some(x) => x.as_u64().ok_or("\"version\" is not a number")?,
    };
    if version <= 1 {
        let v1: V1 = serde_json::from_value(v).map_err(|e| format!("v1: {e}"))?;
        let mut cfg = Config::default();
        cfg.dock.pins = v1.pins;
        cfg.hide_taskbar = v1.hide_taskbar;
        if !cfg.hide_taskbar { cfg.set_shortcut("dock_apps", "none"); }
        cfg.welcome_done = welcome_done;
        cfg.migrate_shortcuts(None);
        cfg.settle_removed();
        return Ok(Parsed { cfg, migrated: true });
    }
    let had_effects = obj.contains_key("effects");
    // "Status icons on every display" became "Same on every display".
    let had_same = obj.get("topbar").and_then(|t| t.get("same_on_all_displays")).is_some();
    let mut cfg: Config = serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
    cfg.version = version as u32;
    cfg.unify_agent_notifications();
    cfg.migrate_shortcuts(Some(&v));
    migrate_removed(&mut cfg, &v);
    if !had_same && cfg.topbar.tray_on_all_monitors {
        cfg.topbar.same_on_all_displays = true;
    }
    cfg.topbar.tray_on_all_monitors = false;
    if !had_effects {
        // A file from before the effects page: its minimize effect and
        // bounce stay as chosen ("custom" unless they are High's own).
        cfg.effects = EffectsCfg::default();
        if !crate::effects::high_keeps(&cfg) {
            cfg.effects.preset = "custom".into();
        }
    }
    migrate_font(&mut cfg);
    cfg.welcome_done = welcome_done;
    cfg.read_only = cfg.version > VERSION;
    // A newer build's file is shown as it is (never rewritten from here).
    if !cfg.read_only {
        cfg.settle_removed();
    }
    let native_version = obj.get("native_defaults_version").and_then(|v| v.as_u64()).unwrap_or(0);
    let migrated = !cfg.read_only && native_version < u64::from(NATIVE_DEFAULTS_VERSION);
    if !cfg.read_only && native_version < 1 {
        if cfg.shortcut("search") == "alt+space" { cfg.set_shortcut("search", "none"); }
        if cfg.hide_taskbar && cfg.shortcut("dock_apps") == "none" { cfg.set_shortcut("dock_apps", "win"); }
        cfg.widgets.hide_from_capture = false;
        cfg.companion.download_notifications = false;
    }
    if !cfg.read_only && native_version < 2 {
        if matches!(cfg.dock.displays.trim(), "" | "follow") { cfg.dock.displays = "every".into(); }
        cfg.focus_timer.dnd = true;
        cfg.native_defaults_version = 2;
    }
    // Compare the stored values, not deserialization defaults. Missing
    // fields already use today's defaults. A v4 file is never migrated.
    let same = |section: &str, key: &str, old: serde_json::Value| {
        obj.get(section).and_then(|s| s.get(key)).is_some_and(|v| {
            if old.is_number() { v.as_f64() == old.as_f64() } else { v == &old }
        })
    };
    if !cfg.read_only && native_version < 3 {
        if same("dock", "icon_size", 52.into()) { cfg.dock.icon_size = DOCK_ICON_DEFAULT; }
        if same("dock", "max_scale", 1.4.into()) { cfg.dock.max_scale = MAGNIFY_OFF; }
        if same("dock", "indicator", "dot".into()) { cfg.dock.indicator = Indicator::Bar; }
        if same("dock", "squircle", true.into()) { cfg.dock.squircle = false; }
        if same("dock", "show_trash", true.into()) { cfg.dock.show_trash = false; }
        if same("animation", "bounce", true.into()) { cfg.animation.bounce = false; }
        if same("animation", "launch", "bounce".into()) { cfg.animation.launch = "none".into(); }
        if same("topbar", "show_app_menus", true.into()) { cfg.topbar.show_app_menus = false; }
        let legacy_clear = obj.get("topbar").and_then(|t| t.get("height_defaults")).and_then(|v| v.as_u64()).unwrap_or(0) < TOPBAR_HEIGHT_DEFAULTS as u64;
        if same("topbar", "style", "auto".into()) || (legacy_clear && same("topbar", "style", "clear".into())) { cfg.topbar.style = "frost".into(); }
        for display in cfg.dock.per_display.values_mut() {
            if display.icon_size == Some(52.0) { display.icon_size = Some(DOCK_ICON_DEFAULT); }
            if display.max_scale == Some(1.4) { display.max_scale = Some(MAGNIFY_OFF); }
        }
        cfg.native_defaults_version = 3;
    }
    if !cfg.read_only && native_version < 4 {
        // Every dock shows every running app (owner, 2026-11-20: apps on
        // other displays looked closed). v2 turned the filter on for
        // everyone, so a stored "on" is that old default.
        if same("dock", "display_apps_only", true.into()) { cfg.dock.display_apps_only = false; }
        cfg.native_defaults_version = 4;
    }
    if !cfg.read_only && native_version < 5 {
        if cfg.shortcut("focus_dock") == "none" { cfg.set_shortcut("focus_dock", "win+t"); }
        cfg.native_defaults_version = 5;
    }
    if migrated {
        if cfg.topbar.style.trim().eq_ignore_ascii_case("frost") {
            cfg.topbar.style = "glass".into();
        }
        cfg.native_defaults_version = NATIVE_DEFAULTS_VERSION;
    }
    Ok(Parsed { cfg, migrated })
}

/// The small animations' switches before they became one
/// (`effects.small_animations`).
const OLD_SMALL_ANIMATIONS: [&str; 6] = ["popup_fade", "stack_fade", "preview_pop", "split_hint", "island_motion", "shelf_motion"];

/// Reads what older files keep in fields that are gone (they are ignored
/// otherwise and never written again): `dock.follow_mouse` (before
/// `dock.displays`), `topbar.autohide` (on reserved no space: kept as
/// `topbar.reserve_work_area` off), the small animations' separate
/// switches (off if any of them was off: whoever turned some motion off
/// wants less of it; one left out counts as its default, on), and "Reduce
/// motion" (read from the small animations) as Low-spec mode: whoever
/// turned it on wanted lighter. "Lower automatically" (the preset "auto")
/// and `effects.lower_on_battery` turn nothing on.
fn migrate_removed(cfg: &mut Config, v: &serde_json::Value) {
    let dock = v.get("dock");
    let displays_set = dock.and_then(|d| d.get("displays")).and_then(|x| x.as_str()).is_some_and(|s| !s.trim().is_empty());
    if !displays_set && dock.and_then(|d| d.get("follow_mouse")).and_then(|x| x.as_bool()) == Some(false) {
        cfg.dock.displays = DISPLAY_MODES[DisplayMode::Main.index()].to_string();
    }
    if let Some(fx) = v.get("effects").and_then(|e| e.as_object()) {
        let old = OLD_SMALL_ANIMATIONS.iter().any(|k| fx.contains_key(*k));
        if old && !fx.contains_key("small_animations") {
            cfg.effects.small_animations = !OLD_SMALL_ANIMATIONS.iter().any(|k| fx.get(*k).and_then(|x| x.as_bool()) == Some(false));
        }
        if !fx.contains_key("low_spec") {
            cfg.low_spec_from_reduce_motion();
        }
    }
    let topbar = v.get("topbar");
    // The Windows top bar (26), the owner's call (2026-11-11, the
    // design redo): a file from before it (no height_defaults) holding the
    // old default 32 reads as 26, so an upgrade gets the new look (older
    // builds wrote the default too: a chosen 32 cannot be told apart, and
    // Settings > Top bar > Height brings it back). Any other height was
    // the user's and stays. Once only: the marker is written from here on,
    // so a 32 chosen after this stays. (A newer build's format is kept as
    // it wrote it.)
    let since = topbar.and_then(|t| t.get("height_defaults")).and_then(|x| x.as_u64()).unwrap_or(0);
    if since < TOPBAR_SINCE_26 && cfg.topbar.height == TOPBAR_HEIGHT_OLD {
        cfg.topbar.height = TOPBAR_HEIGHT;
    }
    // The style following the finish (owner, 2026-11-12): a file from
    // before it holding the old default "clear" (written whether chosen or
    // not) reads as "auto" once; frost, recipe and solid were chosen and
    // stay. (A file without the key deserializes to auto already.)
    if v.get("native_defaults_version").and_then(|v| v.as_u64()).unwrap_or(0) < 3
        && since < TOPBAR_HEIGHT_DEFAULTS as u64 && cfg.topbar.style.trim().eq_ignore_ascii_case("clear") {
        cfg.topbar.style = TOPBAR_STYLE_AUTO.into();
    }
    cfg.topbar.height_defaults = cfg.topbar.height_defaults.max(TOPBAR_HEIGHT_DEFAULTS);
    if topbar.and_then(|t| t.get("reserve_work_area")).is_none() && topbar.and_then(|t| t.get("autohide")).and_then(|x| x.as_bool()) == Some(true) {
        cfg.topbar.reserve_work_area = false;
    }
}

/// An appearance block from before the built-in Pretendard (no
/// FONT_DEFAULTS): its "" font was Windows' UI font as the default ("System
/// default" in the old list), which the file cannot tell apart from a
/// choice. The owner's call (2026-11-11, the design redo): nothing named
/// was chosen, so the new default applies: "" stays "" (the built-in
/// Pretendard), and Settings > Appearance > "Windows font" brings the old
/// look back. A family by name is the user's and is kept as it is; a
/// "system" this build wrote (FONT_DEFAULTS) is an explicit choice and is
/// kept too. The marker is written from here on.
fn migrate_font(cfg: &mut Config) {
    // (A newer build's own format is kept as it wrote it.)
    cfg.appearance.font_defaults = cfg.appearance.font_defaults.max(FONT_DEFAULTS);
}

pub fn from_json(text: &str) -> Result<Config, String> {
    parse(text).map(|p| p.cfg)
}

/// How long a read-modify-write waits for another writer.
const LOCK_WAIT_MS: u32 = 3_000;

/// A per-user named mutex around every write of config.json and every
/// read-modify-write of it (stability review: the dock's background saver,
/// its UI-thread saves and the settings window used to race, and one could
/// lose the other's change). A Windows mutex is re-entrant on one thread,
/// so `save` inside `update` takes it again without waiting; other threads
/// and processes wait. Named after the file, so tests never meet the real
/// one.
pub struct WriteLock(windows::Win32::Foundation::HANDLE);

impl WriteLock {
    pub fn take() -> Option<WriteLock> {
        Self::take_for(&file())
    }

    /// The same lock for another file of ours: one mutex per file, named
    /// after it.
    pub fn take_for(path: &std::path::Path) -> Option<WriteLock> {
        Self::take_within(path, LOCK_WAIT_MS)
    }

    fn take_within(path: &std::path::Path, wait_ms: u32) -> Option<WriteLock> {
        use windows::core::HSTRING;
        use windows::Win32::Foundation::{CloseHandle, WAIT_ABANDONED, WAIT_OBJECT_0};
        use windows::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
        let name = format!("Local\\NopBar.Config.{:016x}", crate::icons::fnv64(path.to_string_lossy().to_lowercase().as_bytes()));
        unsafe {
            let m = CreateMutexW(None, false, &HSTRING::from(name)).ok()?;
            // Abandoned = the holder died: ours now.
            let r = WaitForSingleObject(m, wait_ms);
            if r == WAIT_OBJECT_0 || r == WAIT_ABANDONED {
                Some(WriteLock(m))
            } else {
                let _ = CloseHandle(m);
                if wait_ms > 0 {
                    crate::log!("config: another writer held the settings file too long");
                }
                None
            }
        }
    }
}

impl Drop for WriteLock {
    fn drop(&mut self) {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::ReleaseMutex;
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}

/// One read-modify-write of config.json, whole under the write lock: the
/// newest file (or `fallback` if it is missing, unreadable or read-only),
/// changed by `edit`, saved. Returns the result, whether it was saved, and
/// write_seq() right after (so a later write anywhere makes it stale).
pub fn update(fallback: &Config, edit: impl FnOnce(&mut Config)) -> (Config, Result<(), String>, u64) {
    let lock = WriteLock::take();
    let mut latest = read_current().filter(|c| c.writable()).unwrap_or_else(|| fallback.clone());
    edit(&mut latest);
    // Saved, and handed back, without redundant dividers (whoever edited
    // the pins: the dock, the settings, Launchpad).
    latest.settle_dividers();
    let saved = if lock.is_some() { latest.save() } else { Err("the settings file is busy".into()) };
    let seq = write_seq();
    (latest, saved, seq)
}

/// Largest config.json read (audit A13): a real one is a few KB; a huge
/// file is not ours and is never pulled into memory whole.
pub const MAX_FILE_BYTES: u64 = 4 << 20;

/// A file's bytes, at most `max` of them: a bigger file (also one that grew
/// after opening) is an InvalidData error, never read whole.
pub fn read_capped(path: &std::path::Path, max: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(path)?.take(max + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > max {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("larger than {max} bytes")));
    }
    Ok(buf)
}

/// config.json's text (capped; None if missing, too big or not UTF-8).
pub fn read_text() -> Option<String> {
    String::from_utf8(read_capped(&file(), MAX_FILE_BYTES).ok()?).ok()
}

/// The config as it is on disk right now (None if missing or unreadable).
pub fn read_current() -> Option<Config> {
    let text = read_text()?;
    parse(&text).ok().map(|p| p.cfg)
}

/// The pins a new config starts with (Launchpad, then the taskbar's).
pub fn imported_pins() -> Vec<PinEntry> {
    first_pins(pinned::load().into_iter().map(|p| PinEntry::new(p.name, p.path)).collect())
}

/// A new config's pins: Launchpad first, then the taskbar's.
pub fn first_pins(taskbar: Vec<PinEntry>) -> Vec<PinEntry> {
    let mut pins = vec![PinEntry::new(crate::launchpad::PIN_NAME.into(), crate::launchpad::PIN.into())];
    pins.extend(taskbar.into_iter().filter(|p| !crate::launchpad::is_pin(&p.path)));
    pins
}

/// Moves a bad config.json aside. False if that failed (then the original
/// must not be overwritten).
fn back_up_bad_file(why: &str) -> bool {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let bak = app_dir().join(format!("config.json.bak-{secs}"));
    match std::fs::rename(file(), &bak) {
        Ok(()) => {
            crate::log!("config: unreadable ({why}); kept as {}, starting from defaults", bak.display());
            true
        }
        Err(e) => {
            crate::log!("config: unreadable ({why}) and could not be backed up ({e}); not touching it");
            false
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let mut read_only = false;
        match read_capped(&file(), MAX_FILE_BYTES) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                crate::log!("config: none yet, importing taskbar pins");
            }
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                read_only = !back_up_bad_file(&e.to_string());
            }
            Err(e) => {
                // Exists but cannot be read (locked, permissions): leave it.
                crate::log!("config: cannot read ({e}); using defaults without saving");
                read_only = true;
            }
            Ok(bytes) => {
                let parsed = String::from_utf8(bytes)
                    .map_err(|_| "not UTF-8".to_string())
                    .and_then(|text| parse(&text).map(|p| (p, text)));
                match parsed {
                    Ok((mut p, text)) => {
                        LOADED.store(text_hash(&text), Ordering::SeqCst);
                        if p.migrated {
                            crate::log!("config: migrated schema/defaults to v{VERSION}/{} ({} pins)", p.cfg.native_defaults_version, p.cfg.dock.pins.len());
                            let _ = p.cfg.save();
                        } else if !p.cfg.read_only && crate::agenda::scrub_feed_names(&mut p.cfg.calendar.feeds) {
                            // A calendar's name an older build took from its
                            // feed held a piece of its secret link: saved
                            // clean, once.
                            crate::log!("config: calendar names cleaned");
                            let _ = p.cfg.save();
                        }
                        if p.cfg.read_only {
                            crate::log!("config: version {} is newer than this build; read-only", p.cfg.version);
                        }
                        return p.cfg;
                    }
                    Err(why) => read_only = !back_up_bad_file(&why),
                }
            }
        }
        // First run, or a bad file that was moved aside: start from defaults
        // with the user's taskbar pins.
        let mut cfg = Config::default();
        cfg.dock.pins = imported_pins();
        cfg.read_only = read_only;
        let _ = cfg.save();
        cfg
    }

    /// Whether save() may write this config.
    pub fn writable(&self) -> bool {
        !self.read_only && self.version <= VERSION
    }

    /// Every pinned list (the main dock's, each display's own) without
    /// redundant dividers (normalize_dividers). True if any changed.
    pub fn settle_dividers(&mut self) -> bool {
        let mut changed = false;
        let lists = std::iter::once(&mut self.dock.pins).chain(self.dock.per_display.values_mut().filter_map(|d| d.pins.as_mut()));
        for pins in lists {
            let settled = normalize_dividers(pins);
            if settled.len() != pins.len() {
                *pins = settled;
                changed = true;
            }
        }
        changed
    }

    /// The text written to config.json: this config, its pinned lists
    /// settled (settle_dividers) whichever way it is saved.
    pub fn file_text(&self) -> Result<String, String> {
        let mut c = self.clone();
        c.settle_dividers();
        serde_json::to_string_pretty(&c).map_err(|e| e.to_string())
    }

    /// Writes the file (temp file + rename) under the write lock. Err if
    /// it did not land.
    pub fn save(&self) -> Result<(), String> {
        if !self.writable() {
            crate::log!("config: not saving (read-only / newer version {})", self.version);
            return Err("read-only".into());
        }
        let _lock = WriteLock::take().ok_or("the settings file is busy")?;
        let _ = std::fs::create_dir_all(app_dir());
        let text = self.file_text()?;
        LAST_WRITTEN.store(text_hash(&text), Ordering::SeqCst);
        WRITE_SEQ.fetch_add(1, Ordering::SeqCst);
        // Per process: even a writer that could not get the lock never
        // renames or removes another one's temp file.
        let tmp = file().with_extension(format!("json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, file()).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            e.to_string()
        })
    }
}

#[cfg(test)]
mod declutter_tests {
    use super::*;

    /// `v` at "a.b" (None: not there).
    fn at<'a>(v: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
        path.split('.').try_fold(v, |v, k| v.get(k))
    }

    /// Another value of the same kind.
    fn other(v: &serde_json::Value) -> serde_json::Value {
        match v {
            serde_json::Value::Bool(b) => serde_json::Value::Bool(!b),
            serde_json::Value::Number(n) => match n.as_u64() {
                Some(u) => serde_json::json!(u + 1),
                None => serde_json::json!(n.as_f64().unwrap_or(0.0) + 1.0),
            },
            serde_json::Value::String(s) => serde_json::Value::String(format!("{s}x")),
            x => x.clone(),
        }
    }

    /// A file that set every removed row's field (and an old preset) still
    /// reads, each such field back at its default, and reads the same again
    /// once saved: old files keep loading, removed rows behave as default.
    #[test]
    fn removed_rows_read_back_as_defaults() {
        let def = serde_json::to_value(Config::default()).unwrap();
        let mut old = def.clone();
        for (path, _) in DEFAULTED {
            let (sec, key) = path.split_once('.').unwrap();
            let v = at(&def, path).unwrap_or_else(|| panic!("{path}: not a field"));
            old[sec][key] = other(v);
            assert_ne!(at(&old, path), at(&def, path), "{path}");
        }
        old["effects"]["preset"] = "low".into();
        old["effects"]["magnify"] = false.into();
        old["effects"]["spring"] = false.into();
        old["animation"]["minimize"] = "genie".into();
        // A key no build knows: ignored.
        old["effects"]["sparkles"] = true.into();
        let c = from_json(&old.to_string()).expect("an old file still loads");
        let got = serde_json::to_value(&c).unwrap();
        for (path, rule) in DEFAULTED {
            let (was, now, default) = (at(&old, path), at(&got, path), at(&def, path));
            match rule {
                Settle::Default => assert_eq!(now, default, "{path}: the default"),
                Settle::OffStays => {
                    assert_eq!(default, Some(&serde_json::Value::Bool(true)), "{path}: on by default");
                    assert_eq!(now, was, "{path}: turned off, stays off");
                }
                Settle::Kept => assert_eq!(now, was, "{path}: kept"),
                Settle::Own => {}
            }
        }
        assert_eq!(c.effects.preset, "low", "the preset is left as set (its limits too)");
        assert!(c.effects.magnify && c.dock.max_scale == MAGNIFY_OFF, "Effects' magnification off: the Dock's Off");
        assert_eq!(c.dock.indicator, Indicator::Bar);
        assert!(!c.effects.spring && c.animation.minimize == Minimize::Genie, "motion items left as set");
        let again = from_json(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(serde_json::to_value(&again).unwrap(), got, "saved and read again: the same");
        // A new install needs no settling.
        let mut d = Config::default();
        d.settle_removed();
        assert_eq!(serde_json::to_value(&d).unwrap(), def);
    }

    /// Effects' magnification switch is gone: off there becomes every
    /// dock's magnification Off, once; on leaves the docks as they are.
    #[test]
    fn effects_magnify_off_moves_to_the_docks_once() {
        let c = from_json(r#"{"version":2,"effects":{"magnify":false},"dock":{"max_scale":2.0,"per_display":{"A":{"name":"A","max_scale":1.6},"B":{"name":"B","autohide":true}}}}"#).unwrap();
        assert!(c.effects.magnify, "the switch is on from now on");
        assert_eq!(c.dock.max_scale, MAGNIFY_OFF);
        assert_eq!(c.dock.per_display["A"].max_scale, Some(MAGNIFY_OFF), "a display's own magnification too");
        assert_eq!(c.dock.per_display["B"].max_scale, None, "a display that follows the main dock still does");
        // Saved and read again: nothing more moves (the user may turn it
        // back on in the Dock's row).
        let mut again = from_json(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(again.dock.max_scale, MAGNIFY_OFF);
        again.dock.max_scale = 1.6;
        let kept = from_json(&serde_json::to_string(&again).unwrap()).unwrap();
        assert_eq!(kept.dock.max_scale, 1.6, "once only");
        // On (or missing): the docks keep their own.
        let on = from_json(r#"{"version":2,"dock":{"max_scale":2.0}}"#).unwrap();
        assert_eq!((on.dock.max_scale, on.effects.magnify), (2.0, true));
        // The agenda switch is gone too: on, whatever the file said (the
        // calendar links are what shows it).
        assert!(from_json(r#"{"version":2,"calendar":{"agenda":false}}"#).unwrap().calendar.agenda);
    }

    /// The owner's call (2026-11-11, the design redo): an older file's ""
    /// font (Windows' UI font as the default, nothing named) reads as the
    /// new default, the built-in Pretendard, and stays so once saved and
    /// read again; a family by name stays; "Windows font" chosen in this
    /// build (the marked format) stays Windows' font.
    #[test]
    fn an_older_files_empty_font_becomes_pretendard() {
        use crate::fonts::{pick, BUNDLED, SYSTEM_CHOICE, THEME_DEFAULT};
        let all = |_: &str| true;
        let old = from_json(r#"{"version":2,"appearance":{"font":"","text_size":"large"}}"#).unwrap();
        assert_eq!((old.appearance.font.as_str(), old.appearance.text_size.as_str()), ("", "large"));
        assert_eq!(pick(&old.appearance.font, THEME_DEFAULT, "Malgun Gothic", all), BUNDLED, "the new default");
        let again = from_json(&serde_json::to_string(&old).unwrap()).unwrap();
        assert_eq!(again.appearance, old.appearance, "saved and read again");
        assert_eq!(serde_json::to_value(&again).unwrap()["appearance"]["font_defaults"], FONT_DEFAULTS);
        // A family by name stays, and a value of the wrong type (read as
        // "") is the default.
        let named = from_json(r#"{"version":2,"appearance":{"font":"Consolas"}}"#).unwrap();
        assert_eq!(named.appearance.font, "Consolas");
        assert_eq!(pick(&named.appearance.font, THEME_DEFAULT, "Malgun Gothic", all), "Consolas");
        assert_eq!(from_json(r#"{"version":2,"appearance":{"font":7}}"#).unwrap().appearance.font, "");
        // The new format: "Windows font" chosen is kept, drawn in Windows'
        // font, both from a file and saved and read again.
        let marked = from_json(r#"{"version":2,"appearance":{"font":"system","font_defaults":2}}"#).unwrap();
        assert_eq!(marked.appearance.font, SYSTEM_CHOICE);
        assert_eq!(pick(&marked.appearance.font, THEME_DEFAULT, "Malgun Gothic", all), "Malgun Gothic", "Windows' font");
        let chosen_system = Config { appearance: AppearanceCfg { font: SYSTEM_CHOICE.into(), ..AppearanceCfg::default() }, ..Config::default() };
        assert_eq!(from_json(&serde_json::to_string(&chosen_system).unwrap()).unwrap().appearance.font, SYSTEM_CHOICE);
        // Written by this build: "" is the built-in Pretendard, both ways.
        let back = from_json(&serde_json::to_string(&Config::default()).unwrap()).unwrap();
        assert_eq!(back.appearance.font, "");
        assert_eq!(pick(&back.appearance.font, THEME_DEFAULT, "Malgun Gothic", all), BUNDLED);
        // No font list yet (no block), or a v1 file: the new default.
        assert_eq!(from_json(r#"{"version":2}"#).unwrap().appearance.font, "");
        assert_eq!(from_json(r#"{"pins":[]}"#).unwrap().appearance.font, "");
    }

    /// The finish is Flat (opaque) or Glass (it blurs). Old Solid (the
    /// blur off) and old Flat (translucent: the blur on) read as Flat, the
    /// blur switch left as it was; the old Glass (one glass) and the glass
    /// v3 trio's Frost, Glass and Clear (0.1.63) are Glass, written back as
    /// "glass"; a glass without the blur (it drew solid) reads as Flat.
    #[test]
    fn old_finishes_read_as_flat_or_glass() {
        let read = |finish: &str, blur: bool| from_json(&format!(r#"{{"version":2,"theme":{{"finish":"{finish}"}},"effects":{{"blur":{blur}}}}}"#)).unwrap();
        // Old Solid: Flat, the blur still off (never turned on).
        let solid = read("flat", false);
        assert_eq!((solid.finish_choice(), solid.effects.blur), (0, false));
        // Old Flat (translucent): Flat, and nothing blurs.
        let flat = read("flat", true);
        assert_eq!(flat.finish_choice(), 0);
        assert!(!crate::effects::items_of(&flat).blur, "Flat is opaque");
        // Old Glass: Glass (B), blurred.
        let glass = read("glass", true);
        assert_eq!((glass.finish_choice(), FINISHES[glass.finish_choice()]), (1, "glass"));
        assert_eq!(glass.theme.load(std::path::Path::new(".")).finish, crate::theme::Finish::Glass);
        assert!(crate::effects::items_of(&glass).blur);
        // 0.1.63's Frost and Clear (and Glass, any case): Glass, written
        // back as "glass", blurred.
        for f in ["frost", "clear", "glass", "Frost", " CLEAR "] {
            let g = read(f, true);
            assert_eq!((g.finish_choice(), g.theme.finish.as_str()), (1, "glass"), "{f:?}");
            assert_eq!(g.theme.load(std::path::Path::new(".")).finish, crate::theme::Finish::Glass, "{f:?}");
            assert!(crate::effects::items_of(&g).blur, "{f:?}");
            let back = from_json(&serde_json::to_string(&g).unwrap()).unwrap();
            assert_eq!(back.theme.finish, "glass", "{f:?}");
        }
        // A glass without the blur (shown as Solid): Flat, the blur still
        // off; never chosen without the blur, too.
        for f in ["glass", "frost", "clear", ""] {
            let off = read(f, false);
            assert_eq!((off.finish_choice(), off.effects.blur), (0, false), "{f:?}");
        }
        // The choice: a glass turns the blur on, Flat stops using it.
        let mut c = Config::default();
        assert_eq!(c.finish_choice(), FINISH_DEFAULT, "Glass by default");
        assert!(crate::effects::items_of(&c).blur, "a new install blurs");
        for i in 1..FINISH_CHOICES {
            c.set_finish_choice(0);
            c.effects.blur = false;
            c.set_finish_choice(i);
            assert!(c.effects.blur && c.glass() && crate::effects::items_of(&c).blur, "{i}");
        }
        c.set_finish_choice(9);
        assert_eq!(c.finish_choice(), 1, "out of range: the last");
        c.set_finish_choice(0);
        assert!(!c.glass() && !crate::effects::items_of(&c).blur);
    }

    /// Choices that became switches: what an old file chose reads as on
    /// or off; switches made one are on only where every part was on.
    #[test]
    fn choices_that_became_switches_read_back() {
        let c = from_json(r#"{"version":2,"dock":{"indicator":"bar","wheel_desktops":false},"topbar":{"show_desktops":"always","wheel_desktops":true},"input":{"indicator":"always"},"focus_timer":{"chime":false,"toast":true}}"#).unwrap();
        assert_eq!(c.dock.indicator, Indicator::Bar, "a chosen bar stays a bar");
        assert_eq!((c.topbar.desktops_mode(), c.input.indicator_mode()), (0, 0), "Always is Auto");
        assert!(!c.dock.wheel_desktops && !c.topbar.wheel_desktops, "off on the dock: off on both");
        assert!(!c.focus_timer.chime && !c.focus_timer.toast, "a silent notification never becomes a sound");
        let off = from_json(r#"{"version":2,"dock":{"indicator":"none"},"topbar":{"show_desktops":"never"}}"#).unwrap();
        assert_eq!((off.dock.indicator, off.topbar.desktops_mode()), (Indicator::None, 2));
    }

    /// Review P1/P2: reading a file never turns on anything that was
    /// turned off, for every switch in the config (on by default, set off),
    /// and the merged switches' every combination.
    #[test]
    fn reading_never_turns_on_what_was_off() {
        let def = serde_json::to_value(Config::default()).unwrap();
        let mut bools = Vec::new();
        fn walk(v: &serde_json::Value, p: &str, out: &mut Vec<String>) {
            if let Some(m) = v.as_object() {
                for (k, x) in m {
                    walk(x, &if p.is_empty() { k.clone() } else { format!("{p}.{k}") }, out);
                }
            } else if v == &serde_json::Value::Bool(true) {
                out.push(p.to_string());
            }
        }
        walk(&def, "", &mut bools);
        assert!(bools.len() > 20, "{bools:?}");
        for path in &bools {
            let mut old = def.clone();
            let mut cur = &mut old;
            for k in path.split('.') {
                cur = cur.get_mut(k).unwrap();
            }
            *cur = false.into();
            let got = serde_json::to_value(from_json(&old.to_string()).unwrap()).unwrap();
            if path == "hot_corners.enabled" {
                continue;
            }
            // Effects' magnification off is every dock's magnification
            // Off now (what was off stays off, in the Dock's row).
            if path == "effects.magnify" {
                assert_eq!(at(&got, "dock.max_scale").and_then(|v| v.as_f64()), Some(MAGNIFY_OFF as f64), "{path}: the docks' Off");
                continue;
            }
            // The agenda's switch is the calendar links now (the owner's
            // call, settings structure audit 2.6): with no link nothing shows.
            if path == "calendar.agenda" {
                continue;
            }
            assert_eq!(at(&got, path), Some(&serde_json::Value::Bool(false)), "{path}: turned off, must stay off");
        }
        // Automatic updates: on only if checking and installing both were.
        for (check, install) in [(true, true), (true, false), (false, true), (false, false)] {
            let c = from_json(&format!(r#"{{"version":2,"updates":{{"auto_check":{check},"auto_install":{install}}}}}"#)).unwrap();
            assert_eq!((c.updates.auto_check, c.updates.auto_install), (check && install, check && install), "{check} {install}");
        }
        let c = from_json(r#"{"version":2,"updates":{"auto_install":false}}"#).unwrap();
        assert!(!c.updates.auto_check, "a missing check is on, the refused install wins");
        assert!(from_json(r#"{"version":2}"#).unwrap().updates.auto_check, "neither said: on");
        // Scrolling on the dock and on the bar.
        for (dock, bar) in [(true, true), (true, false), (false, true), (false, false)] {
            let c = from_json(&format!(r#"{{"version":2,"dock":{{"wheel_desktops":{dock}}},"topbar":{{"wheel_desktops":{bar}}}}}"#)).unwrap();
            assert_eq!((c.dock.wheel_desktops, c.topbar.wheel_desktops), (dock && bar, dock && bar));
        }
        // The timer's chime and notification.
        for (chime, toast) in [(true, true), (true, false), (false, true), (false, false)] {
            let c = from_json(&format!(r#"{{"version":2,"focus_timer":{{"chime":{chime},"toast":{toast}}}}}"#)).unwrap();
            assert_eq!(c.focus_timer.chime, chime, "the sound as it was");
            assert!(!c.focus_timer.toast);
        }
        // Motion items turned off one by one stay off.
        let c = from_json(r#"{"version":2,"effects":{"small_animations":true},"animation":{"bounce":false,"minimize":"none"}}"#).unwrap();
        assert!(!c.animation.bounce && c.animation.minimize == Minimize::None && c.effects.small_animations);
        // Hot corners paused by the master switch: no corner acts.
        let c = from_json(r#"{"version":2,"hot_corners":{"enabled":false,"top_left":"lock","bottom_right":"desktop"}}"#).unwrap();
        assert!(c.hot_corners.enabled && !c.hot_corners.any(), "paused corners stay quiet");
        let c = from_json(r#"{"version":2,"hot_corners":{"enabled":true,"top_left":"lock"}}"#).unwrap();
        assert_eq!(c.hot_corners.top_left, CornerAction::Lock, "corners in use keep acting");
    }

    /// "Reduce motion" on becomes Low-spec mode; "Lower automatically"
    /// (on in every older file) turns nothing on. Old files load as they
    /// are, and read the same once saved.
    #[test]
    fn low_spec_reads_from_reduce_motion_never_from_auto() {
        // Reduce motion on (the small animations off), the old Low preset.
        let low = from_json(r#"{"version":2,"effects":{"preset":"low","small_animations":false}}"#).unwrap();
        assert!(low.effects.low_spec, "wanted lighter");
        assert!(low.effects.spring, "the other motion items as the file has them (default here)");
        assert_eq!(low.effects.preset, "low", "left as set");
        // Auto on (a new install's default), with its battery switch.
        let auto = from_json(r#"{"version":2,"effects":{"preset":"auto","lower_on_battery":true,"small_animations":true}}"#).unwrap();
        assert!(!auto.effects.low_spec, "Auto does not turn Low-spec mode on");
        assert_eq!(crate::effects::preset_name(&auto), "high", "Auto's items are High's, kept as they are");
        assert_eq!(crate::effects::resolve(&auto, crate::effects::Env { animations: true, high_contrast: false }).items, crate::effects::items_of(&auto));
        let saved = serde_json::to_string(&auto).unwrap();
        assert!(!saved.contains("lower_on_battery"), "the gone switch is not written again");
        // Auto off ("custom") with Reduce motion off: off.
        let custom = from_json(r#"{"version":2,"effects":{"preset":"custom","small_animations":true}}"#).unwrap();
        assert!(!custom.effects.low_spec);
        // No effects at all: off.
        assert!(!from_json(r#"{"version":2}"#).unwrap().effects.low_spec);
        // Once written, the file's own value wins (no migration again).
        let mut on = Config::default();
        on.set_low_spec(true);
        let back = from_json(&serde_json::to_string(&on).unwrap()).unwrap();
        assert!(back.effects.low_spec);
        let off = from_json(r#"{"version":2,"effects":{"low_spec":false,"small_animations":false}}"#).unwrap();
        assert!(!off.effects.low_spec, "turned off by hand stays off");
        assert_eq!(serde_json::to_value(from_json(&saved).unwrap()).unwrap(), serde_json::to_value(&auto).unwrap(), "saved and read again: the same");
    }

    /// The switch only switches: custom animation choices (genie, no
    /// bounce) are what they were after on -> off, and after a save and
    /// a read.
    #[test]
    fn low_spec_switch_never_rewrites_the_animation_items() {
        let mut c = from_json(r#"{"version":2,"effects":{"preset":"custom","low_spec":false,"small_animations":true,"spring":false},"animation":{"minimize":"genie","bounce":false}}"#).unwrap();
        let before = c.clone();
        c.set_low_spec(true);
        let on = crate::effects::resolve(&c, crate::effects::Env { animations: true, high_contrast: false });
        assert_eq!(on.items.minimize, Minimize::None, "in force: the lightest");
        assert_eq!((c.animation.minimize, c.animation.bounce, c.effects.spring), (Minimize::Genie, false, false), "stored: untouched");
        let read = from_json(&serde_json::to_string(&c).unwrap()).unwrap();
        assert!(read.effects.low_spec && read.animation.minimize == Minimize::Genie && !read.animation.bounce);
        c.set_low_spec(false);
        assert_eq!(c, before, "off: exactly what was chosen");
        let read = from_json(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(read, before, "saved and read again");
        // The same file from before the mode (no low_spec yet): only the
        // small animations on, so nothing to move and Low-spec mode off.
        let old = from_json(r#"{"version":2,"effects":{"preset":"custom","small_animations":true,"spring":false},"animation":{"minimize":"genie","bounce":false}}"#).unwrap();
        assert!(!old.effects.low_spec && old.animation.minimize == Minimize::Genie && !old.animation.bounce && !old.effects.spring);
    }

    /// "Reduce motion" (every animation item off) read once as Low-spec
    /// mode with the items back at a new install's; only some of them off
    /// (older separate switches, a hand-made file) stay as they are.
    #[test]
    fn reduce_motion_moves_to_low_spec_once() {
        let rm = r#"{"version":2,"effects":{"preset":"custom","small_animations":false,"spring":false,"label_fade":false,"launchpad_motion":false},"animation":{"bounce":false,"minimize":"none"}}"#;
        let mut r = from_json(rm).unwrap();
        assert!(r.effects.low_spec);
        assert!(r.effects.small_animations && r.effects.spring && r.effects.label_fade && r.effects.launchpad_motion && !r.animation.bounce && r.animation.minimize == Minimize::Shrink);
        // Once: written with low_spec, it is never moved again.
        r.set_low_spec(false);
        r.effects.spring = false;
        let again = from_json(&serde_json::to_string(&r).unwrap()).unwrap();
        assert!(!again.effects.low_spec && !again.effects.spring);
        // Only the small animations off: Low-spec mode, the rest as set.
        let some = from_json(r#"{"version":2,"effects":{"small_animations":false},"animation":{"minimize":"genie"}}"#).unwrap();
        assert!(some.effects.low_spec && !some.effects.small_animations && some.animation.minimize == Minimize::Genie);
        assert!(!Config::default().motion_all_off());
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn windows_feel_defaults_v3_install_migrate_and_preserve() {
        let new = Config::default();
        assert_eq!((new.native_defaults_version, new.dock.icon_size, new.dock.max_scale), (6, 44.0, 1.0));
        assert!(!new.dock.display_apps_only, "every dock shows every running app");
        assert_eq!(new.dock.indicator, Indicator::Bar);
        assert!(!new.dock.squircle && !new.dock.show_trash && !new.animation.bounce && !new.topbar.show_app_menus);
        assert_eq!(new.topbar.style, "glass");
        let old = from_json(r#"{"version":2,"native_defaults_version":2,"dock":{"icon_size":52.0,"max_scale":1.4,"indicator":"dot","squircle":true,"show_trash":true,"per_display":{"A":{"icon_size":52,"max_scale":1.4},"B":{"icon_size":64,"max_scale":2.0}}},"animation":{"bounce":true,"launch":"bounce"},"topbar":{"style":"auto","show_app_menus":true}}"#).unwrap();
        assert_eq!((old.dock.icon_size, old.dock.max_scale, old.dock.indicator), (44.0, 1.0, Indicator::Bar));
        assert!(!old.dock.squircle && !old.dock.show_trash && !old.animation.bounce && !old.topbar.show_app_menus);
        assert_eq!(old.topbar.style, "glass");
        assert_eq!((old.dock.per_display["A"].icon_size, old.dock.per_display["A"].max_scale), (Some(44.0), Some(1.0)));
        assert_eq!((old.dock.per_display["B"].icon_size, old.dock.per_display["B"].max_scale), (Some(64.0), Some(2.0)));
        let chosen = from_json(r#"{"version":2,"native_defaults_version":2,"dock":{"icon_size":60,"max_scale":1.6,"indicator":"none"},"animation":{"bounce":false},"topbar":{"style":"solid","show_app_menus":false}}"#).unwrap();
        assert_eq!((chosen.dock.icon_size, chosen.dock.max_scale, chosen.dock.indicator), (60.0, 1.6, Indicator::None));
        assert_eq!(chosen.topbar.style, "solid");
        let current = from_json(r#"{"version":2,"native_defaults_version":3,"dock":{"icon_size":52,"max_scale":1.4,"indicator":"dot","squircle":true,"show_trash":true},"animation":{"bounce":true},"topbar":{"style":"clear","show_app_menus":true}}"#).unwrap();
        assert_eq!((current.dock.icon_size, current.dock.max_scale, current.dock.indicator), (52.0, 1.4, Indicator::Dot));
        assert!(current.dock.squircle && current.dock.show_trash && current.animation.bounce && current.topbar.show_app_menus);
        assert_eq!(current.topbar.style, "clear");
        let roundtrip = from_json(&serde_json::to_string(&current).unwrap()).unwrap();
        assert_eq!(roundtrip.dock, current.dock);
    }
    #[test]
    fn native_defaults_migrate_once_and_preserve_choices() {
        let old = super::parse(r#"{"version":2,"shortcuts":{"search":"alt+space","dock_apps":"none"},"widgets":{"hide_from_capture":true},"companion":{"download_notifications":true}}"#).unwrap();
        assert!(old.migrated);
        assert_eq!(old.cfg.shortcut("search"), "none");
        assert_eq!(old.cfg.shortcut("dock_apps"), "win");
        assert!(!old.cfg.widgets.hide_from_capture && !old.cfg.companion.download_notifications);
        let custom = super::parse(r#"{"version":2,"hide_taskbar":false,"shortcuts":{"search":"ctrl+space","dock_apps":"alt"}}"#).unwrap().cfg;
        assert_eq!(custom.shortcut("search"), "ctrl+space");
        assert_eq!(custom.shortcut("dock_apps"), "alt");
        assert_eq!(super::parse(r#"{"version":2,"hide_taskbar":false}"#).unwrap().cfg.shortcut("dock_apps"), "none");
        let mut changed = old.cfg;
        changed.set_shortcut("search", "alt+space");
        changed.set_shortcut("dock_apps", "none");
        changed.widgets.hide_from_capture = true;
        changed.companion.download_notifications = true;
        let again = super::parse(&serde_json::to_string(&changed).unwrap()).unwrap();
        assert!(!again.migrated);
        assert_eq!(again.cfg, changed);
        let future = super::parse(r#"{"version":999,"widgets":{"hide_from_capture":true},"shortcuts":{"search":"alt+space"}}"#).unwrap();
        assert!(!future.migrated && future.cfg.read_only);
        assert!(future.cfg.widgets.hide_from_capture);
        assert_eq!(future.cfg.shortcut("search"), "alt+space");
    }
    #[test]
    fn notification_settings_migrate_all_four_legacy_combinations() {
        for hook in [false, true] {
            for bubble in [false, true] {
                let input = serde_json::json!({
                    "version": 2,
                    "agents": { "claude": hook, "codex": hook },
                    "chat": { "claude_mcp": bubble, "codex_mcp": bubble, "antigravity_mcp": bubble }
                });
                let c = super::parse(&input.to_string()).unwrap().cfg;
                assert_eq!((c.agents.claude, c.chat.claude_mcp), (hook || bubble, hook || bubble));
                assert_eq!((c.agents.codex, c.chat.codex_mcp), (hook || bubble, hook || bubble));
                assert_eq!(c.chat.antigravity_mcp, bubble);
                let saved = serde_json::to_string(&c).unwrap();
                assert_eq!(super::parse(&saved).unwrap().cfg, c);
            }
        }
    }

    use super::*;

    #[test]
    fn legacy_agent_island_keys_are_ignored() {
        for on in [false, true] {
            let cfg: Config = serde_json::from_value(serde_json::json!({
                "agents": {"claude": true, "codex": true, "keep_awake": false, "bar": on, "card": on}
            })).unwrap();
            assert_eq!(cfg.agents, AgentsCfg { claude: true, codex: true, keep_awake: false });
            let saved = serde_json::to_value(&cfg).unwrap();
            assert!(saved["agents"].get("bar").is_none());
            assert!(saved["agents"].get("card").is_none());
        }
    }

    #[test]
    fn reads_are_capped() {
        let dir = std::env::temp_dir().join(format!("nopbar-cap-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let f = dir.join("c.json");
        std::fs::write(&f, b"0123456789").unwrap();
        assert_eq!(read_capped(&f, 10).unwrap().len(), 10);
        assert_eq!(read_capped(&f, 9).unwrap_err().kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(read_capped(&dir.join("none"), 9).unwrap_err().kind(), std::io::ErrorKind::NotFound);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn icon_overrides_stay_in_the_icons_folder() {
        assert_eq!(icon_file("..\\outside.png"), None);
        assert_eq!(icon_file("icons\\..\\x.png"), None);
        assert_eq!(icon_file("C:\\x.png"), None);
        assert_eq!(icon_file("\\\\server\\share\\x.png"), None);
        assert_eq!(icon_file("icons\\sub\\x.png"), None);
        assert_eq!(icon_file("icons\\missing-file.png"), None);
    }

    #[test]
    fn migrate_v1_keeps_pins_and_taskbar() {
        let v1 = r#"{"pins":[{"name":"A","path":"C:\\a.lnk"},{"name":"B","path":"shell:AppsFolder\\X!App"}],"hide_taskbar":false}"#;
        let cfg = from_json(v1).unwrap();
        assert_eq!(cfg.version, VERSION);
        assert_eq!(cfg.dock.pins.len(), 2);
        assert_eq!(cfg.dock.pins[1].path, "shell:AppsFolder\\X!App");
        assert!(!cfg.hide_taskbar);
        assert_eq!(cfg.dock.icon_size, 44.0);
    }

    #[test]
    fn web_app_pins_roundtrip() {
        let web = crate::webapps::WebPin {
            aumid: "Chrome._crx_cmkncekebbplodngbpllndjkfo".into(),
            exe: r"C:\Program Files\Google\Chrome\Application\chrome_proxy.exe".into(),
            args: "--profile-directory=Default --app-id=cmkncekebbplodngbpllndjkfo".into(),
            icon: None,
            name: Some("Chrome 원격 데스크톱".into()),
        };
        let mut cfg = Config::default();
        cfg.dock.pins = vec![
            PinEntry { name: "Chrome 원격 데스크톱".into(), path: crate::webapps::apps_folder_path(&web.aumid), web: Some(web.clone()) },
            PinEntry::new("N".into(), "C:\\n.exe".into()),
        ];
        let text = serde_json::to_string(&cfg).unwrap();
        assert_eq!(text.matches("\"web\"").count(), 1, "only web apps carry it");
        let back = from_json(&text).unwrap();
        assert_eq!(back.dock.pins, cfg.dock.pins);
        // Pins written before: no web details, as they were.
        let old = from_json(r#"{"version":2,"dock":{"pins":[{"name":"A","path":"C:\\a.lnk"}]}}"#).unwrap();
        assert_eq!(old.dock.pins, vec![PinEntry::new("A".into(), "C:\\a.lnk".into())]);
    }

    #[test]
    fn dividers_are_pins_with_their_own_paths() {
        let a = PinEntry::new("A".into(), "C:\\a.lnk".into());
        let d1 = PinEntry::divider(std::slice::from_ref(&a));
        let d2 = PinEntry::divider(&[a.clone(), d1.clone()]);
        assert!(d1.is_divider() && d2.is_divider() && !a.is_divider());
        assert!(d1.name.is_empty() && d1.web.is_none());
        assert!(!d1.path.eq_ignore_ascii_case(&d2.path), "each divider has its own path");
        assert!(is_divider_path("NopBar:Divider:1a") && !is_divider_path("nopbar:launchpad") && !is_divider_path("nopbar:div"));
        // Round trip: written as a plain pin entry, read back the same.
        let mut cfg = Config::default();
        cfg.dock.pins = vec![a.clone(), d1.clone(), PinEntry::new("B".into(), "C:\\b.exe".into())];
        let text = serde_json::to_string(&cfg).unwrap();
        assert!(text.contains(&format!("{{\"name\":\"\",\"path\":\"{}\"}}", d1.path)), "{text}");
        assert_eq!(from_json(&text).unwrap().dock.pins, cfg.dock.pins);
        // Per display too.
        cfg.dock.per_display.insert("D2".into(), DisplayDock { pins: Some(vec![a.clone(), d2.clone(), a.clone()]), ..Default::default() });
        let back = from_json(&serde_json::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(back.dock.per_display["D2"].pins.as_ref().map(|p| p[1].is_divider()), Some(true));
    }

    #[test]
    fn older_builds_read_a_config_with_dividers() {
        // Older builds' pin entry: name and path required, the rest
        // ignored. A divider reads as a pin there, never as a bad file.
        #[derive(Deserialize)]
        struct OldPin {
            #[allow(dead_code)]
            name: String,
            path: String,
        }
        #[derive(Deserialize)]
        struct OldDock {
            pins: Vec<OldPin>,
        }
        #[derive(Deserialize)]
        struct OldCfg {
            dock: OldDock,
        }
        let mut cfg = Config::default();
        let d = PinEntry::divider(&[]);
        cfg.dock.pins = vec![PinEntry::new("A".into(), "C:\\a.lnk".into()), d.clone(), PinEntry::new("B".into(), "C:\\b.exe".into())];
        let old: OldCfg = serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(old.dock.pins.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(), ["C:\\a.lnk", d.path.as_str(), "C:\\b.exe"]);
        // And a config from before dividers loads unchanged.
        let before = r#"{"version":2,"dock":{"pins":[{"name":"A","path":"C:\\a.lnk"},{"name":"B","path":"C:\\b.exe"}]}}"#;
        let c = from_json(before).unwrap();
        assert!(c.dock.pins.iter().all(|p| !p.is_divider()));
        assert_eq!(c.dock.pins.len(), 2);
    }

    #[test]
    fn redundant_dividers_go() {
        let r = |s: &str| redundant_dividers(&s.chars().map(|c| c == '|').collect::<Vec<_>>());
        assert_eq!(r("a|b"), Vec::<usize>::new());
        assert_eq!(r("|ab"), [0], "not at the start");
        assert_eq!(r("ab|"), Vec::<usize>::new(), "at the end it stays (pinned apps | running ones)");
        assert_eq!(r("a||b"), [2], "never two in a row: the first stays");
        assert_eq!(r("ab||"), [3], "one at the end");
        assert_eq!(r("a|||b|c"), [2, 3]);
        assert_eq!(r("||a||b||"), [0, 1, 4, 7]);
        assert_eq!(r("|"), [0]);
        assert_eq!(r(""), Vec::<usize>::new());
    }

    #[test]
    fn normalize_dividers_by_path() {
        let p = |s: &str| PinEntry::new(s.into(), s.into());
        let (d1, d2) = (PinEntry::new(String::new(), "nopbar:divider:1".into()), PinEntry::new(String::new(), "nopbar:divider:2".into()));
        let paths = |v: Vec<PinEntry>| v.into_iter().map(|e| e.path).collect::<Vec<_>>();
        let list = vec![d1.clone(), p("A"), d2.clone(), p("B")];
        assert_eq!(paths(normalize_dividers(&list)), ["A", "nopbar:divider:2", "B"]);
        // Kept at the end, through a save and a load.
        let list = vec![p("A"), p("B"), d1.clone()];
        assert_eq!(normalize_dividers(&list), list);
        let mut cfg = Config::default();
        cfg.dock.pins = list.clone();
        let back = from_json(&serde_json::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(normalize_dividers(&back.dock.pins), list);
        let list = vec![p("A"), d1.clone(), d2.clone()];
        assert_eq!(paths(normalize_dividers(&list)), ["A", "nopbar:divider:1"]);
        // Nothing to drop: the same list.
        let list = vec![p("A"), d1, p("B"), d2, p("C")];
        assert_eq!(normalize_dividers(&list), list);
    }

    #[test]
    fn every_save_settles_the_dividers() {
        let p = |s: &str| PinEntry::new(s.into(), s.into());
        let d = |n: u32| PinEntry::new(String::new(), format!("nopbar:divider:{n}"));
        let mut c = Config::default();
        // As the settings left it: B removed from A | B | C, and a divider
        // dragged to the top of a display's own list.
        c.dock.pins = vec![p("A"), d(1), d(2), p("C"), d(3)];
        c.dock.per_display.insert("D2".into(), DisplayDock { pins: Some(vec![d(4), p("X"), d(5)]), ..Default::default() });
        c.dock.per_display.insert("D3".into(), DisplayDock::default());
        let back = from_json(&c.file_text().unwrap()).unwrap();
        assert_eq!(back.dock.pins, vec![p("A"), d(1), p("C"), d(3)], "one of a pair, the end one stays");
        assert_eq!(back.dock.per_display["D2"].pins, Some(vec![p("X"), d(5)]));
        assert_eq!(back.dock.per_display["D3"].pins, None, "a display without its own list keeps none");
        // In memory too (config::update hands back the settled config).
        assert!(c.settle_dividers());
        assert_eq!(c.dock.pins, back.dock.pins);
        assert!(!c.settle_dividers(), "settled: nothing more");
        assert_eq!(redundant_divider_paths(&[d(1), p("A"), d(2), d(3)]), [d(1).path, d(3).path]);
    }

    #[test]
    fn v2_roundtrip() {
        let mut cfg = Config::default();
        cfg.dock.pins.push(PinEntry::new("N".into(), "C:\\n.exe".into()));
        cfg.dock.position = Position::Left;
        cfg.theme.source = "file:themes\\mine.json".into();
        let text = serde_json::to_string(&cfg).unwrap();
        assert!(text.contains("\"ref\":\"file:themes\\\\mine.json\""));
        assert_eq!(from_json(&text).unwrap(), cfg);
    }

    #[test]
    fn custom_accent_is_kept_and_shown_as_custom() {
        let mut t = ThemeRef::default();
        // "" (the theme's) and "system": Automatic, no swatch.
        assert_eq!((t.accent_index(), t.custom_accent()), (0, None));
        t.accent = "system".into();
        assert_eq!(t.accent_index(), 0);
        // A preset: its place; any other color: "Custom…" with its swatch.
        t.accent = "#30d158".into();
        assert_eq!((t.accent_index(), t.custom_accent()), (7, None));
        t.accent = accent_hex(crate::theme::Rgba { r: 0x12, g: 0xAB, b: 0x3C, a: 255 });
        assert_eq!(t.accent, "#12AB3C");
        assert_eq!(t.accent_index(), ACCENT_CUSTOM);
        assert_eq!(t.custom_accent(), Some(crate::theme::Rgba { r: 0x12, g: 0xAB, b: 0x3C, a: 255 }));
        // Choosing "Custom…" again keeps the color; a preset replaces it.
        t.set_accent_index(ACCENT_CUSTOM);
        assert_eq!(t.accent, "#12AB3C");
        t.set_accent_index(1);
        assert_eq!(t.accent, ACCENTS[1].0);
        // "Custom…" from a preset without the dialog: a grey off the list.
        t.set_accent_index(ACCENT_CUSTOM);
        assert_eq!(t.accent_index(), ACCENT_CUSTOM);
        // Garbage: Automatic.
        t.accent = "teal".into();
        assert_eq!((t.accent_index(), t.custom_accent()), (0, None));
        // Saved and read back, and honored in both modes.
        let mut cfg = Config::default();
        cfg.theme.accent = "#12AB3C".into();
        let back = from_json(&serde_json::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(back.theme.accent, "#12AB3C");
        let th = back.theme.load(std::path::Path::new("."));
        for light in [false, true] {
            let r = th.resolve(&crate::syslook::SysLook { light, ..Default::default() });
            assert_eq!(r.accent(), crate::theme::Rgba { r: 0x12, g: 0xAB, b: 0x3C, a: 255 });
        }
    }

    #[test]
    fn finish_defaults_glass_and_round_trips() {
        use crate::theme::Finish;
        // Files that never chose (written "" or from before the setting):
        // the default, Glass.
        for old in [r#"{"version":2,"theme":{"ref":"builtin:graphite"}}"#, r#"{"version":2,"theme":{"ref":"builtin:graphite","finish":""}}"#] {
            let old = from_json(old).unwrap();
            assert_eq!(old.theme.finish_index(), FINISH_DEFAULT);
            assert_eq!(old.theme.load(std::path::Path::new(".")).finish, Finish::Glass);
        }
        let mut cfg = Config::default();
        assert_eq!((cfg.theme.finish.as_str(), cfg.theme.finish_index()), ("glass", 1), "a new install: Glass");
        for (i, want) in [Finish::Flat, Finish::Glass].into_iter().enumerate() {
            cfg.theme.finish = FINISHES[i].into();
            let back = from_json(&serde_json::to_string(&cfg).unwrap()).unwrap();
            assert_eq!(back, cfg);
            assert_eq!(back.theme.finish_index(), i);
            assert_eq!(back.theme.load(std::path::Path::new(".")).finish, want);
        }
        // Unknown text: Flat.
        cfg.theme.finish = "chrome".into();
        assert_eq!((cfg.theme.finish_index(), cfg.theme.load(std::path::Path::new(".")).finish), (0, Finish::Flat));
    }

    #[test]
    fn tray_icon_is_on_by_default_and_kept() {
        assert!(Config::default().tray_icon);
        // Files from before the setting existed show the icon.
        assert!(from_json(r#"{"version":2}"#).unwrap().tray_icon);
        let off = from_json(r#"{"version":2,"tray_icon":false}"#).unwrap();
        assert!(!off.tray_icon);
        assert!(!from_json(&serde_json::to_string(&off).unwrap()).unwrap().tray_icon);
    }

    #[test]
    fn safely_remove_settings_default_and_kept() {
        let d = Config::default().topbar;
        assert!(!d.tray_hotplug && d.removable_dot);
        // Older files: explorer's icon dropped, the dot on.
        let old = from_json(r#"{"version":2,"topbar":{"height":30}}"#).unwrap().topbar;
        assert!(!old.tray_hotplug && old.removable_dot);
        let set = from_json(r#"{"version":2,"topbar":{"tray_hotplug":true,"removable_dot":false}}"#).unwrap();
        let back = from_json(&serde_json::to_string(&set).unwrap()).unwrap().topbar;
        assert!(back.tray_hotplug && !back.removable_dot);
    }

    #[test]
    fn top_bar_tray_mirror_is_on_by_default_and_kept() {
        assert!(Config::default().topbar.tray);
        // Files from before the switch existed keep their tray icons.
        assert!(from_json(r#"{"version":2,"topbar":{"height":30}}"#).unwrap().topbar.tray);
        let off = from_json(r#"{"version":2,"topbar":{"tray":false}}"#).unwrap();
        assert!(!off.topbar.tray);
        assert!(!from_json(&serde_json::to_string(&off).unwrap()).unwrap().topbar.tray);
    }

    #[test]
    fn unknown_fields_ignored_and_missing_defaulted() {
        let cfg = from_json(r#"{"version":2,"future":1,"dock":{"icon_size":60,"weird":true}}"#).unwrap();
        assert_eq!(cfg.dock.icon_size, 60.0);
        assert_eq!(cfg.dock.max_scale, 1.0);
        assert!(cfg.hide_taskbar);
        assert_eq!(cfg.shortcut("search"), "none");
        let cfg = from_json(r#"{"version":2,"search":{"hotkey":"none"}}"#).unwrap();
        assert_eq!(cfg.shortcut("search"), "none");
    }

    #[test]
    fn bars_by_default_and_indicator_choices_survive() {
        // The glass shelf's dots (fable review 6): new files and files that
        // never said; the indicator is a switch now (a dot or none).
        assert_eq!(Config::default().dock.indicator, Indicator::Bar);
        assert_eq!(from_json(r#"{"version":2,"dock":{"icon_size":60}}"#).unwrap().dock.indicator, Indicator::Bar);
        assert_eq!(from_json(r#"{"version":2,"dock":{"indicator":"bar"}}"#).unwrap().dock.indicator, Indicator::Bar);
        assert_eq!(from_json(r#"{"version":2,"dock":{"indicator":"none"}}"#).unwrap().dock.indicator, Indicator::None);
    }

    #[test]
    fn update_checks_default_on_and_round_trip() {
        assert!(from_json(r#"{"version":2}"#).unwrap().updates.auto_check, "older files: on");
        let cfg = from_json(r#"{"version":2,"updates":{"auto_check":false}}"#).unwrap();
        assert!(!cfg.updates.auto_check);
        assert_eq!(from_json(&serde_json::to_string(&cfg).unwrap()).unwrap(), cfg);
    }

    #[test]
    fn corrupt_json_is_error() {
        assert!(from_json("{\"pins\": [").is_err());
    }

    #[test]
    fn own_write_detection() {
        let text = "{\"version\":2}";
        LAST_WRITTEN.store(text_hash(text), Ordering::SeqCst);
        assert!(is_own_write(text));
        assert!(!is_own_write("{\"version\":2 }"));
    }

    #[test]
    fn non_object_json_rejected() {
        for t in ["null", "[]", "\"x\"", "3"] {
            assert!(parse(t).is_err(), "{t} must be rejected");
        }
    }

    #[test]
    fn v1_wrong_types_rejected() {
        assert!(parse(r#"{"pins":[],"hide_taskbar":"false"}"#).is_err());
        assert!(parse(r#"{"pins":"x"}"#).is_err());
        assert!(parse(r#"{"hide_taskbar":false}"#).is_err(), "v1 without pins is not a v1 file");
    }

    #[test]
    fn explicit_version_1_is_migrated() {
        let p = parse(r#"{"version":1,"pins":[],"hide_taskbar":false}"#).unwrap();
        assert!(p.migrated);
        assert_eq!(p.cfg.version, VERSION);
        assert!(!p.cfg.hide_taskbar);
    }

    #[test]
    fn display_mode_reads_old_files() {
        // Older files have only follow_mouse: read once, never written.
        let p = parse(r#"{"version":2,"dock":{"follow_mouse":false}}"#).unwrap();
        assert_eq!(p.cfg.dock.display_mode(), DisplayMode::Main);
        assert!(!serde_json::to_string(&p.cfg).unwrap().contains("follow_mouse"));
        let p = parse(r#"{"version":2,"dock":{"follow_mouse":true}}"#).unwrap();
        assert_eq!(p.cfg.dock.display_mode(), DisplayMode::Every);
        assert_eq!(DockCfg::default().display_mode(), DisplayMode::Every);
        // The new value wins; unknown text reads as follow.
        let p = parse(r#"{"version":2,"dock":{"follow_mouse":true,"displays":"every"}}"#).unwrap();
        assert_eq!(p.cfg.dock.display_mode(), DisplayMode::Every);
        let p = parse(r#"{"version":2,"dock":{"follow_mouse":false,"displays":"sideways"}}"#).unwrap();
        assert_eq!(p.cfg.dock.display_mode(), DisplayMode::Follow);
        let mut d = DockCfg::default();
        for (i, m) in [DisplayMode::Follow, DisplayMode::Main, DisplayMode::Every].into_iter().enumerate() {
            d.set_display_mode(i);
            assert_eq!(d.display_mode(), m);
            assert_eq!(m.index(), i);
        }
        // The app filter needs both the switch and a dock on every display.
        for (i, only) in [(0, true), (1, true), (2, true), (0, false), (2, false)] {
            d.set_display_mode(i);
            d.display_apps_only = only;
            assert_eq!(d.only_display_apps(), only && i == 2, "mode {i}, switch {only}");
        }
    }

    #[test]
    fn v4_shows_every_running_app_on_every_dock_once() {
        // v2 and v3 files hold the old forced "on": it turns off once.
        for v in [2, 3] {
            let p = parse(&format!(r#"{{"version":2,"native_defaults_version":{v},"dock":{{"displays":"every","display_apps_only":true,"icon_size":52}}}}"#)).unwrap();
            assert!(p.migrated && !p.cfg.dock.display_apps_only, "v{v}");
            assert_eq!(p.cfg.native_defaults_version, 6);
        }
        // A v3 file's other values are not migrated again.
        let v3 = parse(r#"{"version":2,"native_defaults_version":3,"dock":{"icon_size":52,"indicator":"dot"}}"#).unwrap();
        assert_eq!((v3.cfg.dock.icon_size, v3.cfg.dock.indicator), (52.0, Indicator::Dot));
        // Turned on again after v4: kept.
        let chosen = parse(r#"{"version":2,"native_defaults_version":4,"dock":{"displays":"every","display_apps_only":true}}"#).unwrap();
        assert!(chosen.cfg.dock.only_display_apps());
    }

    #[test]
    fn focus_dock_v5_migrates_only_the_old_default() {
        for (version, saved, want) in [(4, "none", "win+t"), (4, "ctrl+alt+d", "ctrl+alt+d"), (5, "none", "none")] {
            let json = format!(r#"{{"version":2,"native_defaults_version":{version},"shortcuts":{{"focus_dock":"{saved}"}}}}"#);
            assert_eq!(parse(&json).unwrap().cfg.shortcut("focus_dock"), want);
        }
    }

    #[test]
    fn current_native_defaults_are_not_migrated() {
        assert!(!parse(&serde_json::to_string(&Config::default()).unwrap()).unwrap().migrated);
    }

    #[test]
    fn native_decisions_migrate_v1_once_without_repeating_bundle_one() {
        let p = parse(r#"{"version":2,"native_defaults_version":1,"shortcuts":{"search":"alt+space","dock_apps":"none"},"widgets":{"hide_from_capture":true},"companion":{"download_notifications":true},"dock":{"displays":"follow","display_apps_only":false},"focus_timer":{"dnd":false}}"#).unwrap();
        assert!(p.migrated);
        assert_eq!(p.cfg.native_defaults_version, 6);
        assert_eq!(p.cfg.dock.display_mode(), DisplayMode::Every);
        assert!(!p.cfg.dock.only_display_apps() && p.cfg.focus_timer.dnd);
        assert!(p.cfg.widgets.hide_from_capture && p.cfg.companion.download_notifications);
        assert_eq!(p.cfg.shortcut("search"), "alt+space");
        assert_eq!(p.cfg.shortcut("dock_apps"), "none");
        let main = parse(r#"{"version":2,"native_defaults_version":1,"dock":{"displays":"main"}}"#).unwrap();
        assert_eq!(main.cfg.dock.display_mode(), DisplayMode::Main);
        let mut custom = p.cfg;
        custom.dock.displays = "follow".into();
        custom.dock.display_apps_only = false;
        custom.focus_timer.dnd = false;
        let again = parse(&serde_json::to_string(&custom).unwrap()).unwrap();
        assert!(!again.migrated);
        assert_eq!(again.cfg, custom);
    }

    #[test]
    fn launchpad_defaults() {
        let c = Config::default();
        assert_eq!((c.shortcut("launchpad").as_str(), c.shortcut("launchpad_win_tap").as_str()), ("none", "none"));
        // An older file's keys: read.
        let c = from_json(r#"{"version":2,"launchpad":{"hotkey":"alt+f1","win_key":true}}"#).unwrap();
        assert_eq!((c.shortcut("launchpad").as_str(), c.shortcut("launchpad_win_tap").as_str()), ("alt+f1", "win"));
        let c = from_json(r#"{"version":2,"launchpad":{"win_key":true}}"#).unwrap();
        assert_eq!(c.shortcut("launchpad"), "none", "a missing key is the default");
        // A new config pins Launchpad first (once).
        let taskbar = vec![PinEntry::new("Edge".into(), "C:\\edge.lnk".into()), PinEntry::new("x".into(), "NopBar:Launchpad".into())];
        let pins = first_pins(taskbar);
        assert_eq!(pins.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(), [crate::launchpad::PIN, "C:\\edge.lnk"]);
        assert_eq!(first_pins(Vec::new()).len(), 1);
    }

    #[test]
    fn launchpad_layout_schema() {
        // Default: name order, nothing written.
        let c = Config::default();
        assert!(c.launchpad.layout.is_empty());
        assert!(serde_json::to_value(&c).unwrap().get("launchpad").is_none(), "the default is not written");
        // Pages of app ids and folders, read and written the same.
        let text = r#"{"version":2,"launchpad":{"layout":[["a!App",{"folder":"게임","apps":["b","c"]}],["d"]]}}"#;
        let c = from_json(text).unwrap();
        assert_eq!(
            c.launchpad.layout,
            vec![vec![LpItem::App("a!App".into()), LpItem::folder("게임", vec!["b".into(), "c".into()])], vec![LpItem::App("d".into())]]
        );
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["launchpad"]["layout"][0][1]["folder"], "게임");
        assert_eq!(v["launchpad"]["layout"][0][1]["apps"][1], "c");
        assert_eq!(v["launchpad"]["layout"][1][0], "d");
        assert_eq!(from_json(&serde_json::to_string(&c).unwrap()).unwrap(), c, "round trip");
        // A folder's session id is never written, and not part of equality.
        let mut f = LpItem::folder("x", vec!["a".into()]);
        if let LpItem::Folder { id, .. } = &mut f {
            *id = 7;
        }
        assert_eq!(f, LpItem::folder("x", vec!["a".into()]));
        assert!(!serde_json::to_string(&f).unwrap().contains('7'));
        // An older file's launchpad section (hotkey, Windows key): no layout,
        // and its keys still reach the shortcuts.
        let old = from_json(r#"{"version":2,"launchpad":{"hotkey":"alt+f1","win_key":true}}"#).unwrap();
        assert!(old.launchpad.layout.is_empty());
        assert_eq!(old.shortcut("launchpad"), "alt+f1");
        // Old keys next to a layout: both read.
        let both = from_json(r#"{"version":2,"launchpad":{"hotkey":"alt+f1","layout":[["x"]]}}"#).unwrap();
        assert_eq!((both.shortcut("launchpad").as_str(), both.launchpad.layout.len()), ("alt+f1", 1));
        // Odd shapes never make the file unreadable.
        for odd in [
            r#"{"version":2,"launchpad":5}"#,
            r#"{"version":2,"launchpad":{"layout":"nope"}}"#,
            r#"{"version":2,"launchpad":{"layout":[5,{"x":1},["a",7,{"folder":3},{"apps":"no"}]]}}"#,
        ] {
            let c = from_json(odd).unwrap_or_else(|e| panic!("{odd}: {e}"));
            for page in &c.launchpad.layout {
                for item in page {
                    if let LpItem::App(id) = item {
                        assert!(id.is_empty() || id == "a", "{odd}: {id}");
                    }
                }
            }
        }
        let c = from_json(r#"{"version":2,"launchpad":{"layout":[["a",{"apps":["b",1]}]]}}"#).unwrap();
        assert_eq!(c.launchpad.layout[0][1], LpItem::folder("", vec!["b".into()]), "a folder without a name, odd ids skipped");
    }

    #[test]
    fn launchpad_library_fields() {
        // Defaults: schema 0 (an older file), the Library on, nothing else.
        let c = Config::default();
        assert_eq!((c.launchpad.schema, c.launchpad.show_library, c.launchpad.cleanup_dismissed), (0, true, false));
        // An older file (layout only) reads as schema 0: it migrates on open.
        let old = from_json(r#"{"version":2,"launchpad":{"layout":[["a"]]}}"#).unwrap();
        assert_eq!((old.launchpad.schema, old.launchpad.show_library), (0, true));
        // Every field read and written back.
        let text = r#"{"version":2,"launchpad":{"layout":[["a"]],"schema":1,"show_library":false,"seen":["a","b"],"library":{"b":"games"},"hidden":["c"],"cleanup_dismissed":true}}"#;
        let c = from_json(text).unwrap();
        let lp = &c.launchpad;
        assert_eq!(lp.schema, LP_LIBRARY_SCHEMA);
        assert!(!lp.show_library && lp.cleanup_dismissed);
        assert_eq!((lp.seen.len(), lp.library.get("b").map(String::as_str), lp.hidden.as_slice()), (2, Some("games"), &["c".to_string()][..]));
        assert_eq!(from_json(&serde_json::to_string(&c).unwrap()).unwrap(), c, "round trip");
        // Defaults are not written.
        let mut d = Config::default();
        d.launchpad.schema = 1;
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!(v["launchpad"], serde_json::json!({"schema": 1}));
        // Odd shapes read as defaults.
        let odd = from_json(r#"{"version":2,"launchpad":{"schema":"x","show_library":3,"seen":[1,"a",""],"library":{"":"dev","x":5,"y":"office"},"hidden":"no","cleanup_dismissed":"yes"}}"#).unwrap();
        let lp = &odd.launchpad;
        assert_eq!((lp.schema, lp.show_library, lp.cleanup_dismissed), (0, true, false));
        assert_eq!(lp.seen, vec!["a".to_string()]);
        assert_eq!(lp.library.len(), 1, "only y");
        assert!(lp.hidden.is_empty());
    }

    #[test]
    fn hot_corners_default_to_none() {
        let c = Config::default();
        assert_eq!(c.hot_corners.all(), [CornerAction::None; 4]);
        assert!(!c.hot_corners.any());
        // Missing from an older file: none, the switch on.
        assert_eq!(from_json(r#"{"version":2}"#).unwrap().hot_corners, HotCornersCfg::default());
        assert!(c.hot_corners.enabled && !c.hot_corners.active());
        // Older files without the switch keep their corners working.
        let old = from_json(r#"{"version":2,"hot_corners":{"top_left":"lock"}}"#).unwrap();
        assert!(old.hot_corners.enabled && old.hot_corners.active());
        assert!(from_json(r#"{"version":2}"#).unwrap().focus_timer.enabled);
    }

    #[test]
    fn hot_corners_round_trip() {
        let mut cfg = Config::default();
        cfg.hot_corners = HotCornersCfg {
            desktop_click: true,
            enabled: true,
            top_left: CornerAction::TaskView,
            top_right: CornerAction::ControlCenter,
            bottom_left: CornerAction::ScreenSaver,
            bottom_right: CornerAction::Desktop,
        };
        let text = serde_json::to_string(&cfg).unwrap();
        assert!(text.contains(r#""hot_corners":{"desktop_click":true,"enabled":true,"top_left":"task_view","top_right":"control_center","bottom_left":"screen_saver","bottom_right":"desktop"}"#), "{text}");
        assert_eq!(from_json(&text).unwrap(), cfg);
        for a in CornerAction::ALL {
            let json = serde_json::to_string(&a).unwrap();
            assert_eq!(json, format!("\"{}\"", a.name()));
            assert_eq!(serde_json::from_str::<CornerAction>(&json).unwrap(), a);
        }
    }

    #[test]
    fn desktops_mode_reads_leniently() {
        let mut t = TopbarCfg::default();
        assert_eq!(t.desktops_mode(), 0, "auto by default");
        for (i, m) in DESKTOPS_MODES.iter().enumerate() {
            t.show_desktops = m.to_string();
            assert_eq!(t.desktops_mode(), i);
        }
        t.show_desktops = " Always ".into();
        assert_eq!(t.desktops_mode(), 1);
        t.show_desktops = "sometimes".into();
        assert_eq!(t.desktops_mode(), 0, "unknown = auto");
        // Old files without the field read as auto.
        let cfg = from_json(r#"{"version":2,"topbar":{"height":30}}"#).unwrap();
        assert_eq!(cfg.topbar.desktops_mode(), 0);
    }

    #[test]
    fn unknown_corner_action_reads_as_none() {
        let cfg = from_json(r#"{"version":2,"hot_corners":{"top_left":"search","top_right":"lock","bottom_left":7},"dock":{"icon_size":60}}"#).unwrap();
        assert_eq!(cfg.hot_corners.top_left, CornerAction::None);
        assert_eq!(cfg.hot_corners.top_right, CornerAction::Lock);
        assert_eq!(cfg.hot_corners.bottom_left, CornerAction::None);
        assert_eq!(cfg.dock.icon_size, 60.0, "the rest of the file still counts");
    }

    #[test]
    fn welcome_done_is_inferred_for_older_files() {
        // A new config (as first run saves it) has not been through the flow.
        assert!(!Config::default().welcome_done);
        let fresh = serde_json::to_string(&Config::default()).unwrap();
        assert!(!from_json(&fresh).unwrap().welcome_done, "written by this build: as written");
        // Files from before the flow: their users have set NopBar up already.
        assert!(from_json(r#"{"version":2}"#).unwrap().welcome_done);
        assert!(from_json(r#"{"version":2,"welcomed":true}"#).unwrap().welcome_done);
        assert!(from_json(r#"{"version":2,"welcomed":false,"dock":{"gap":3}}"#).unwrap().welcome_done);
        assert!(from_json(r#"{"pins":[],"hide_taskbar":false}"#).unwrap().welcome_done, "v1");
        // Given: kept either way.
        assert!(!from_json(r#"{"version":2,"welcomed":true,"welcome_done":false}"#).unwrap().welcome_done);
        assert!(from_json(r#"{"version":2,"welcome_done":true}"#).unwrap().welcome_done);
        let mut done = Config::default();
        done.welcome_done = true;
        assert!(from_json(&serde_json::to_string(&done).unwrap()).unwrap().welcome_done);
    }

    #[test]
    fn shortcuts_migrate_from_the_older_keys() {
        // A file from before the registry.
        let c = from_json(r#"{"version":2,"search":{"hotkey":"ctrl+space"},"launchpad":{"hotkey":"alt+f1","win_key":true},"hotkeys":{"win_digits":true,"focus_dock":"ctrl+alt+d"}}"#).unwrap();
        assert_eq!(c.shortcut("search"), "ctrl+space");
        assert_eq!(c.shortcut("launchpad"), "alt+f1");
        assert_eq!(c.shortcut("launchpad_win_tap"), "win");
        assert_eq!(c.shortcut("focus_dock"), "ctrl+alt+d");
        assert_eq!(c.shortcut("dock_apps"), "win");
        // Defaults when nothing is there.
        let d = from_json(r#"{"version":2}"#).unwrap();
        assert_eq!(d.shortcut("search"), "none");
        assert_eq!(d.shortcut("launchpad_win_tap"), "none");
        assert_eq!(d.shortcut("dock_apps"), "win");
        assert_eq!(d.shortcuts, Config::default().shortcuts);
        // The map wins over the old keys once it is there.
        let m = from_json(r#"{"version":2,"search":{"hotkey":"ctrl+space"},"shortcuts":{"search":"ctrl+alt+k"}}"#).unwrap();
        assert_eq!(m.shortcut("search"), "ctrl+alt+k");
        assert!(!serde_json::to_string(&m).unwrap().contains("hotkey"), "the old keys are not written again");
        // Bad values fall back to the old key, then the default.
        let b = from_json(r#"{"version":2,"search":{"hotkey":"ctrl+space"},"shortcuts":{"search":"alt+wat","dock_apps":"meta"}}"#).unwrap();
        assert_eq!(b.shortcut("search"), "ctrl+space");
        assert_eq!(b.shortcut("dock_apps"), "win");
        // Round trip.
        let mut r = Config::default();
        r.set_shortcut("dock_apps", "alt");
        r.set_shortcut("launchpad_win_tap", "win");
        assert_eq!(from_json(&serde_json::to_string(&r).unwrap()).unwrap(), r);
        r.set_shortcut("dock_apps", "WIN");
        assert_eq!(from_json(&serde_json::to_string(&r).unwrap()).unwrap().shortcut("dock_apps"), "win");
        // An older build rewrote the file without the map: its keys count.
        let old = r#"{"version":2,"launchpad":{"hotkey":"ctrl+alt+l","win_key":false}}"#;
        assert_eq!(from_json(old).unwrap().shortcut("launchpad"), "ctrl+alt+l");
        // v1 files get the defaults.
        assert_eq!(from_json(r#"{"pins":[]}"#).unwrap().shortcut("search"), "none");
    }

    /// A file from before the cleanup of 2026-11-03, with every field that
    /// was removed then: it loads, what it meant is kept, and none of them
    /// is written again.
    #[test]
    fn files_with_removed_fields_still_load() {
        let old = r#"{
            "version": 2,
            "dock": {"icon_size": 44, "range": 4.0, "gap": 10, "follow_mouse": false, "show_recent_separator": false},
            "topbar": {"autohide": true, "clock_format": "24h", "runner_character": "cat", "hogs_always": true, "hogs_background": true, "show_runner": true},
            "animation": {"minimize": "genie", "genie_capture": true},
            "hotkeys": {"win_digits": true, "focus_dock": "ctrl+alt+d"},
            "search": {"hotkey": "ctrl+space", "with": "flow"},
            "launchpad": {"hotkey": "alt+f1", "win_key": true},
            "clipboard": {"hotkey": "alt+shift+v", "paste": true},
            "effects": {"preset": "custom", "popup_fade": false, "stack_fade": false, "preview_pop": false, "split_hint": true, "island_motion": false, "shelf_motion": false, "runner_fps": 8, "render": "software"}
        }"#;
        let p = parse(old).unwrap();
        assert!(p.migrated && p.cfg.writable());
        let c = &p.cfg;
        assert_eq!(c.dock.icon_size, 44.0);
        assert_eq!(c.dock.display_mode(), DisplayMode::Main, "follow_mouse off");
        assert!(!c.topbar.reserve_work_area, "autohide on reserved no space; still none");
        // Some small animations were off: Reduce motion read on, so
        // Low-spec mode is on; the other motion items stay as the file has
        // them.
        assert!(c.effects.low_spec);
        assert_eq!(c.animation.minimize, Minimize::Genie);
        assert_eq!(c.shortcut("dock_apps"), "win");
        assert_eq!(c.shortcut("focus_dock"), "ctrl+alt+d");
        assert_eq!(c.shortcut("search"), "ctrl+space");
        assert_eq!(c.search.with, "flow");
        assert_eq!(c.shortcut("launchpad"), "alt+f1");
        assert_eq!(c.shortcut("launchpad_win_tap"), "win");
        assert_eq!(c.effects.preset, "custom");
        assert!(!c.effects.small_animations, "some of them were off: off");
        assert_eq!(c.effects.render, "software");
        let text = serde_json::to_string(c).unwrap();
        for gone in ["\"range\"", "\"gap\"", "follow_mouse", "show_recent_separator", "\"autohide\":true", "clock_format", "runner_character", "hogs_", "genie_capture", "\"hotkeys\"", "\"hotkey\"", "\"launchpad\":{", "win_key", "popup_fade", "stack_fade", "preview_pop", "split_hint", "island_motion", "shelf_motion", "runner_fps", "show_runner"] {
            assert!(!text.contains(gone), "{gone} written again: {text}");
        }
        assert_eq!(from_json(&text).unwrap(), *c, "and it reads back the same");
        // All six off: off. None of them (or the new key given): as is.
        let off = r#"{"version":2,"effects":{"popup_fade":false,"stack_fade":false,"preview_pop":false,"split_hint":false,"island_motion":false,"shelf_motion":false}}"#;
        assert!(!from_json(off).unwrap().effects.small_animations);
        let some = r#"{"version":2,"effects":{"popup_fade":false}}"#;
        assert!(!from_json(some).unwrap().effects.small_animations, "one turned off: off");
        let one_on = r#"{"version":2,"effects":{"split_hint":true}}"#;
        assert!(from_json(one_on).unwrap().effects.small_animations, "none off (the rest left out = on): on");
        let all_on = r#"{"version":2,"effects":{"popup_fade":true,"stack_fade":true,"preview_pop":true,"split_hint":true,"island_motion":true,"shelf_motion":true}}"#;
        assert!(from_json(all_on).unwrap().effects.small_animations);
        let new = r#"{"version":2,"effects":{"popup_fade":true,"small_animations":false}}"#;
        assert!(!from_json(new).unwrap().effects.small_animations, "the new key wins");
        assert!(from_json(r#"{"version":2}"#).unwrap().effects.small_animations);
        // The top bar's space: reserved unless autohide was on; written
        // only when off, read back as written.
        assert!(from_json(r#"{"version":2,"topbar":{"autohide":false}}"#).unwrap().topbar.reserve_work_area);
        assert!(from_json(r#"{"version":2}"#).unwrap().topbar.reserve_work_area);
        let written = |c: &Config| serde_json::to_value(c).unwrap()["topbar"].get("reserve_work_area").cloned();
        assert_eq!(written(c), Some(serde_json::Value::Bool(false)));
        assert_eq!(written(&Config::default()), None, "on (the default) is not written");
        let kept = r#"{"version":2,"topbar":{"autohide":true,"reserve_work_area":true}}"#;
        assert!(from_json(kept).unwrap().topbar.reserve_work_area, "the new key wins");
        // A display choice already made wins over follow_mouse.
        let both = r#"{"version":2,"dock":{"follow_mouse":false,"displays":"every"}}"#;
        assert_eq!(from_json(both).unwrap().dock.display_mode(), DisplayMode::Every);
    }

    /// A file from before the five features were removed (the top bar's
    /// clipboard history button, the shelf, the workspaces, the runner, the
    /// resource monitor), with every field they had: it loads, everything
    /// else in it is kept, none of their settings is written again, and a
    /// removed shortcut's keys are free for another (its entry in the map
    /// is left as it is: nothing reads it).
    #[test]
    fn files_from_before_the_five_removals_still_load() {
        let old = r#"{
            "version": 2,
            "dock": {"icon_size": 52, "autohide": true},
            "topbar": {"height": 30, "show_clipboard": true, "show_shelf": true, "show_workspaces": true, "show_runner": true, "runner_metric": "gpu",
                       "show_monitor": true, "monitor_cpu": false, "monitor_ram": true, "monitor_gpu": true, "monitor_net": false, "monitor_disk": true,
                       "island_screenshot": true, "clock_seconds": true},
            "shelf": {"drag_open": false, "edge": "left"},
            "widgets": {"board": false, "notes": true},
            "shortcuts": {"search": "ctrl+space", "shelf": "ctrl+alt+s", "notes": "win+alt+n"},
            "effects": {"preset": "custom", "small_animations": false}
        }"#;
        let p = parse(old).unwrap();
        assert!(p.migrated && p.cfg.writable());
        let c = &p.cfg;
        // What the file said about everything else.
        assert_eq!((c.dock.icon_size, c.topbar.height), (44.0, 30.0));
        assert!(c.dock.autohide && c.topbar.island_screenshot && c.topbar.clock_seconds);
        assert!(c.widgets.notes);
        assert_eq!(c.shortcut("search"), "ctrl+space");
        assert_eq!(c.effects.preset, "custom");
        assert!(!c.effects.small_animations);
        // Written again without the removed features' fields.
        let text = serde_json::to_string(c).unwrap();
        for gone in ["show_clipboard", "show_shelf", "show_workspaces", "show_runner", "runner_metric", "show_monitor", "monitor_", "\"shelf\":{", "drag_open"] {
            assert!(!text.contains(gone), "{gone} written again: {text}");
        }
        assert_eq!(from_json(&text).unwrap(), *c, "and it reads back the same");
        // The shelf's old shortcut holds nothing: its keys go to another.
        let keys = crate::shortcuts::parse("ctrl+alt+s").unwrap().unwrap();
        assert_eq!(crate::shortcuts::clash(c, crate::shortcuts::NOTES, keys), None);
        let mut c2 = c.clone();
        c2.set_shortcut(crate::shortcuts::NOTES, "ctrl+alt+s");
        assert!(crate::shortcuts::duplicates(&c2).is_empty());
        assert_eq!(from_json(&serde_json::to_string(&c2).unwrap()).unwrap().shortcut(crate::shortcuts::NOTES), "ctrl+alt+s");
    }

    /// A file from before the clipboard board was removed, with its
    /// switches (widgets.board, widgets.board_downloads) and its shortcut
    /// (in the map, and the older clipboard.hotkey): it loads, the other
    /// widgets keep what the file said, the switches are not written
    /// again, and the board's keys are free for another shortcut (its entry
    /// in the map is left as it is: nothing reads it, as the shelf's).
    #[test]
    fn files_from_before_the_board_removal_still_load() {
        let old = r#"{
            "version": 2,
            "dock": {"icon_size": 50},
            "clipboard": {"hotkey": "alt+shift+v"},
            "widgets": {"board": true, "board_downloads": false, "notes": false, "notes_per_desktop": false, "pieces": true, "hide_from_capture": false},
            "shortcuts": {"search": "ctrl+space", "clipboard": "win+alt+v", "notes": "win+alt+n", "snip": "win+alt+s"}
        }"#;
        let p = parse(old).unwrap();
        assert!(p.migrated && p.cfg.writable());
        let c = &p.cfg;
        assert_eq!(c.dock.icon_size, 50.0);
        assert!(!c.widgets.notes && !c.widgets.notes_per_desktop && c.widgets.pieces && !c.widgets.hide_from_capture);
        assert_eq!((c.shortcut("search"), c.shortcut("notes"), c.shortcut("snip")), ("ctrl+space".into(), "win+alt+n".into(), "win+alt+s".into()));
        assert!(crate::shortcuts::find("clipboard").is_none(), "no such shortcut any more");
        let text = serde_json::to_string(c).unwrap();
        for gone in ["\"board\"", "board_downloads", "\"hotkey\""] {
            assert!(!text.contains(gone), "{gone} written again: {text}");
        }
        assert_eq!(from_json(&text).unwrap(), *c, "and it reads back the same");
        // The board's old shortcut holds nothing: its keys go to another.
        let keys = crate::shortcuts::parse("win+alt+v").unwrap().unwrap();
        assert_eq!(crate::shortcuts::clash(c, crate::shortcuts::NOTES, keys), None);
        let mut c2 = c.clone();
        c2.set_shortcut(crate::shortcuts::NOTES, "win+alt+v");
        assert!(crate::shortcuts::duplicates(&c2).is_empty());
        // A new file has no entry for it.
        assert!(!Config::default().shortcuts.contains_key("clipboard"));
    }

    #[test]
    fn effects_migrate_and_round_trip() {
        // Older files: High, unless their animation choices are not High's.
        assert_eq!(from_json(r#"{"version":2}"#).unwrap().effects.preset, "custom");
        assert_eq!(from_json(r#"{"version":2,"animation":{"minimize":"genie"}}"#).unwrap().effects.preset, "custom");
        assert_eq!(from_json(r#"{"version":2,"animation":{"bounce":false}}"#).unwrap().effects.preset, "custom");
        // An old preset: left as set; a removed switch turned off stays
        // off.
        let c = from_json(r#"{"version":2,"effects":{"preset":"low","shadows":false}}"#).unwrap();
        assert_eq!(c.effects.preset, "low");
        assert!(!c.effects.shadows);
        assert!(c.effects.blur, "missing fields are defaults");
        assert_eq!(from_json(&serde_json::to_string(&c).unwrap()).unwrap(), c);
    }

    #[test]
    fn future_version_is_read_only() {
        let p = parse(r#"{"version":3,"dock":{"icon_size":44},"newthing":{"a":1}}"#).unwrap();
        assert_eq!(p.cfg.version, 3);
        assert_eq!(p.cfg.dock.icon_size, 44.0);
        assert!(!p.cfg.writable());
        assert!(Config::default().writable());
    }

    fn pin(p: &str) -> PinEntry {
        PinEntry::new(p.into(), p.into())
    }

    fn every() -> Config {
        let mut c = Config::default();
        c.dock.set_display_mode(2);
        c.dock.pins = vec![pin("a.lnk")];
        c
    }

    #[test]
    fn no_display_map_is_todays_behavior() {
        // A file from before per-display docks: every dock is the main one.
        let p = parse(r#"{"version":2,"dock":{"displays":"every","icon_size":60}}"#).unwrap();
        assert!(p.cfg.dock.per_display.is_empty());
        assert_eq!(p.cfg.for_display(Some("GSM5B09")), p.cfg);
        assert_eq!(p.cfg.dock.for_display(Some("GSM5B09")).icon_size, 60.0);
        // Written back as an empty map (older builds ignore the key).
        let text = serde_json::to_string(&p.cfg).unwrap();
        assert!(text.contains("\"per_display\":{}"), "{text}");
    }

    #[test]
    fn a_display_overrides_only_what_it_sets() {
        let mut c = every();
        c.dock.icon_size = 52.0;
        c.dock.autohide = false;
        c.dock.per_display.insert("D2".into(), DisplayDock { position: Some(Position::Left), icon_size: Some(40.0), ..Default::default() });
        let d = c.dock.for_display(Some("D2"));
        assert_eq!((d.position, d.icon_size), (Position::Left, 40.0), "its own");
        assert!(!d.autohide && d.pins == c.dock.pins && d.max_scale == c.dock.max_scale, "the rest from the main dock");
        // The main dock changes: what D2 leaves out follows, live.
        c.dock.autohide = true;
        c.dock.max_scale = 2.0;
        c.dock.icon_size = 64.0;
        let d = c.dock.for_display(Some("D2"));
        assert!(d.autohide && d.max_scale == 2.0);
        assert_eq!(d.icon_size, 40.0, "not what it sets");
        // Other displays and the main dock: the main settings.
        assert_eq!(c.dock.for_display(Some("D3")).icon_size, 64.0);
        assert_eq!(c.dock.for_display(None).icon_size, 64.0);
        // Only with a dock on every display.
        c.dock.set_display_mode(1);
        assert_eq!(c.dock.for_display(Some("D2")).icon_size, 64.0);
        assert!(c.dock.own(Some("D2")).is_none());
    }

    #[test]
    fn display_entries_round_trip_with_only_their_fields() {
        let mut c = every();
        c.dock.per_display.insert("GSM5B09-ABC".into(), DisplayDock { name: "LG".into(), max_scale: Some(1.2), pins: Some(vec![pin("b.exe")]), ..Default::default() });
        let text = serde_json::to_string_pretty(&c).unwrap();
        assert!(!text.contains("\"icon_size\": null"), "absent fields are left out");
        let back = from_json(&text).unwrap();
        assert_eq!(back.dock.per_display, c.dock.per_display);
        // Hand-written: unknown fields are ignored, missing ones inherit.
        let p = parse(r#"{"version":2,"dock":{"displays":"every","per_display":{"X":{"autohide":true,"color":"red"}}}}"#).unwrap();
        let x = &p.cfg.dock.per_display["X"];
        assert_eq!((x.autohide, x.position, x.pins.as_ref()), (Some(true), None, None));
    }

    #[test]
    fn edits_land_on_the_display_that_made_them() {
        let mut c = every();
        // A display with its own place and apps (not its own size).
        c.dock.per_display.insert("D2".into(), DisplayDock { position: Some(Position::Right), pins: Some(vec![pin("x.exe")]), ..Default::default() });
        // Its dock pins y, moves to the left and grows.
        edit_for_display(&mut c, Some("D2"), |c| {
            c.dock.pins.push(pin("y.exe"));
            c.dock.position = Position::Left;
            c.dock.icon_size = 70.0;
            c.hide_taskbar = false;
        });
        let own = &c.dock.per_display["D2"];
        assert_eq!(own.pins.as_ref().map(|p| p.len()), Some(2));
        assert_eq!(own.position, Some(Position::Left));
        assert_eq!(c.dock.pins, vec![pin("a.lnk")], "the main dock's apps stay");
        assert_eq!(c.dock.position, Position::Bottom, "and its place");
        assert_eq!(c.dock.icon_size, 70.0, "a field it does not set is the main dock's");
        assert_eq!(own.icon_size, None);
        assert!(!c.hide_taskbar, "everything else as usual");
        // A display without its own settings (same as the main dock), or
        // the primary: the main dock's.
        edit_for_display(&mut c, Some("D3"), |c| c.dock.pins.push(pin("z.exe")));
        edit_for_display(&mut c, None, |c| c.dock.autohide = true);
        assert_eq!(c.dock.pins.len(), 2);
        assert!(c.dock.autohide);
        assert!(!c.dock.per_display.contains_key("D3"));
    }

    #[test]
    fn copy_of_takes_the_main_dock_as_it_is() {
        let mut d = DockCfg { position: Position::Right, icon_size: 44.0, max_scale: 1.2, autohide: true, ..Default::default() };
        d.pins = vec![pin("a.lnk")];
        let own = DisplayDock::copy_of(&d, "Laptop");
        let mut merged = DockCfg::default();
        own.apply(&mut merged);
        assert_eq!((merged.position, merged.icon_size, merged.max_scale, merged.autohide), (Position::Right, 44.0, 1.2, true));
        assert_eq!(merged.pins, d.pins);
        assert_eq!(own.name, "Laptop");
    }

    #[test]
    fn status_icons_on_every_display_becomes_same_on_every_display() {
        // The old switch on: read into the new one, never written again.
        let p = parse(r#"{"version":2,"topbar":{"tray_on_all_monitors":true}}"#).unwrap();
        assert!(p.cfg.topbar.same_on_all_displays && p.cfg.topbar.everywhere());
        let text = serde_json::to_string(&p.cfg).unwrap();
        assert!(!text.contains("tray_on_all_monitors"), "{text}");
        assert!(text.contains("\"same_on_all_displays\":true"));
        // The new key wins when both are there (a newer file).
        let p = parse(r#"{"version":2,"topbar":{"tray_on_all_monitors":true,"same_on_all_displays":false}}"#).unwrap();
        assert!(!p.cfg.topbar.same_on_all_displays);
        // Neither: off; the island shows on the display in use.
        let p = parse(r#"{"version":2}"#).unwrap();
        assert!(!p.cfg.topbar.same_on_all_displays);
        assert_eq!(p.cfg.topbar.island_displays(), 0);
        // No row any more: the display in use, whatever the file said.
        let p = parse(r#"{"version":2,"topbar":{"island_displays":"EVERY"}}"#).unwrap();
        assert_eq!(p.cfg.topbar.island_displays(), 0);
        let mut t = TopbarCfg { island_displays: "EVERY".into(), ..Default::default() };
        assert_eq!(t.island_displays(), 1, "read leniently");
        t.island_displays = "moon".into();
        assert_eq!(t.island_displays(), 0, "unknown reads as active");
    }

    #[test]
    fn the_bar_is_26_and_glass_out_of_the_box_and_a_chosen_height_stays() {
        let d = TopbarCfg::default();
        assert_eq!((d.height, d.style.as_str()), (26.0, "glass"));
        assert_eq!((d.style_for(true), d.style_for(false)), (4, 4), "adaptive glass in both finishes");
        assert_eq!(d.height_defaults, TOPBAR_HEIGHT_DEFAULTS);
        // Older files (no height_defaults yet), the owner's call
        // (2026-11-11): the old default 32 reads as 26 once, any other
        // height stays; the style is clear. Saved and read again, each
        // stays as it was read. No height: the new default.
        for (old, now) in [(32.0, 26.0), (28.0, 28.0), (30.0, 30.0), (40.0, 40.0), (24.0, 24.0)] {
            let c = from_json(&format!(r#"{{"version":2,"topbar":{{"height":{old}}}}}"#)).unwrap();
            assert_eq!((c.topbar.height, c.topbar.style()), (now, 4), "{old}");
            let saved = serde_json::to_value(&c).unwrap();
            assert_eq!(saved["topbar"]["height_defaults"], TOPBAR_HEIGHT_DEFAULTS, "{old}: the marker is written");
            let again = from_json(&saved.to_string()).unwrap();
            assert_eq!(again.topbar.height, now, "{old}: saved and read again");
        }
        // The marker is read from the file itself, not the default it
        // deserializes to: an old file with other topbar keys moves too.
        let c = from_json(r#"{"version":2,"topbar":{"height":32,"clock_seconds":true}}"#).unwrap();
        assert_eq!(c.topbar.height, 26.0);
        for text in [r#"{"version":2,"topbar":{}}"#, r#"{"version":2,"topbar":{"clock_seconds":true}}"#, r#"{"version":2}"#] {
            assert_eq!(from_json(text).unwrap().topbar.height, 26.0, "{text}");
        }
        // Once only: after the move, 32 chosen again stays, through any
        // number of saves.
        let mut c = from_json(r#"{"version":2,"topbar":{"height":32}}"#).unwrap();
        assert_eq!(c.topbar.height, 26.0);
        c.topbar.height = 32.0;
        c.topbar.style = TOPBAR_STYLES[2].into();
        let back = from_json(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!((back.topbar.height, back.topbar.style()), (32.0, 2));
        let twice = from_json(&serde_json::to_string(&back).unwrap()).unwrap();
        assert_eq!(twice.topbar.height, 32.0);
        // Written by this build from the start: 32 is someone's choice.
        let mut c = Config::default();
        c.topbar.height = 32.0;
        assert_eq!(from_json(&serde_json::to_string(&c).unwrap()).unwrap().topbar.height, 32.0);
        assert_eq!(from_json(r#"{"version":2,"topbar":{"height":32,"height_defaults":2}}"#).unwrap().topbar.height, 32.0);
        // The style: an older file's "clear" (the old default) reads as
        // auto once; a style chosen stays; written by this build, "clear"
        // is a choice.
        for (text, want) in [
            (r#"{"version":2,"topbar":{"style":"clear"}}"#, "glass"),
            (r#"{"version":2,"topbar":{"style":"clear","height_defaults":2}}"#, "glass"),
            (r#"{"version":2,"topbar":{"style":"frost","height_defaults":2}}"#, "glass"),
            (r#"{"version":2,"topbar":{"style":"solid"}}"#, "solid"),
            (r#"{"version":2,"topbar":{"style":"clear","height_defaults":3}}"#, "clear"),
            (r#"{"version":2}"#, "glass"),
        ] {
            assert_eq!(from_json(text).unwrap().topbar.style, want, "{text}");
        }
        let mut c = Config::default();
        c.topbar.style = "clear".into();
        assert_eq!(from_json(&serde_json::to_string(&c).unwrap()).unwrap().topbar.style, "clear", "chosen, saved and read again");
        assert_eq!(c.topbar.style_for(false), 0, "a chosen clear on Flat");
        // Read leniently; unknown is clear.
        for (text, i) in [("FROST", 1), (" solid ", 3), ("glass", 4), ("unknown", 0)] {
            assert_eq!(TopbarCfg { style: text.into(), ..Default::default() }.style(), i, "{text}");
        }
    }

    #[test]
    fn only_the_previous_default_topbar_style_moves_to_glass() {
        for style in ["frost", "clear", "recipe", "solid", "glass"] {
            let old = format!(r#"{{"version":2,"native_defaults_version":5,"topbar":{{"style":"{style}"}}}}"#);
            let parsed = parse(&old).unwrap();
            assert!(parsed.migrated);
            assert_eq!(parsed.cfg.native_defaults_version, 6);
            assert_eq!(parsed.cfg.topbar.style, if style == "frost" { "glass" } else { style });
            let mut config = parsed.cfg;
            config.topbar.style = style.into();
            let saved = parse(&serde_json::to_string(&config).unwrap()).unwrap();
            assert!(!saved.migrated);
            assert_eq!(saved.cfg.topbar.style, style);
        }
    }

    #[test]
    fn the_clock_shows_seconds_only_when_asked() {
        // Files from before the setting: minutes, as they were.
        let p = parse(r#"{"version":2,"topbar":{"height":32}}"#).unwrap();
        assert!(!p.cfg.topbar.clock_seconds);
        assert!(!Config::default().topbar.clock_seconds);
        let p = parse(r#"{"version":2,"topbar":{"clock_seconds":true}}"#).unwrap();
        assert!(p.cfg.topbar.clock_seconds);
        let text = serde_json::to_string(&p.cfg).unwrap();
        assert!(text.contains("\"clock_seconds\":true"), "{text}");
    }

    #[test]
    fn the_bar_sits_on_top_unless_told_otherwise() {
        // Files from before the setting: on top, as they were.
        let p = parse(r#"{"version":2,"topbar":{"height":32}}"#).unwrap();
        assert_eq!(p.cfg.topbar.position, "top");
        assert!(!p.cfg.topbar.at_bottom());
        assert!(!Config::default().topbar.at_bottom());
        let p = parse(r#"{"version":2,"topbar":{"position":"bottom"}}"#).unwrap();
        assert!(p.cfg.topbar.at_bottom());
        assert_eq!(p.cfg.topbar.position(), 1);
        let p = parse(r#"{"version":2,"topbar":{"position":" Bottom "}}"#).unwrap();
        assert!(p.cfg.topbar.at_bottom(), "case and spaces do not matter");
        let p = parse(r#"{"version":2,"topbar":{"position":"left"}}"#).unwrap();
        assert!(!p.cfg.topbar.at_bottom(), "unknown reads as top");
        // Written back as given.
        let mut c = Config::default();
        c.topbar.position = TOPBAR_POSITIONS[1].into();
        let text = serde_json::to_string(&c).unwrap();
        assert!(text.contains("\"position\":\"bottom\""), "{text}");
    }

    #[test]
    fn a_new_main_display_takes_dock_1_and_the_old_one_keeps_its_dock() {
        let mut c = every();
        c.dock.icon_size = 52.0;
        c.dock.position = Position::Bottom;
        // First sight: only remembered.
        assert!(c.dock.hand_over_main("A", ""));
        assert!(c.dock.per_display.is_empty());
        assert!(!c.dock.hand_over_main("A", ""), "same main: nothing");
        // B had its own dock; then B became the main display.
        c.dock.per_display.insert("B".into(), DisplayDock { name: "B".into(), position: Some(Position::Left), icon_size: Some(40.0), pins: Some(vec![pin("b.exe")]), ..Default::default() });
        assert!(c.dock.hand_over_main("B", "Laptop"));
        assert_eq!(c.dock.main_display, "B");
        // B keeps its dock (now Dock 1's settings)...
        assert_eq!((c.dock.position, c.dock.icon_size), (Position::Left, 40.0));
        assert_eq!(c.dock.pins, vec![pin("b.exe")]);
        assert!(!c.dock.per_display.contains_key("B"));
        // ...and A keeps the one it had, as its own.
        let a = c.dock.for_display(Some("A"));
        assert_eq!((a.position, a.icon_size), (Position::Bottom, 52.0));
        assert_eq!(a.pins, vec![pin("a.lnk")]);
        assert_eq!(c.dock.per_display["A"].name, "Laptop");
        // Back again: both still have their own.
        assert!(c.dock.hand_over_main("A", "B"));
        assert_eq!((c.dock.position, c.dock.icon_size), (Position::Bottom, 52.0));
        let b = c.dock.for_display(Some("B"));
        assert_eq!((b.position, b.icon_size), (Position::Left, 40.0));
        assert!(!c.dock.hand_over_main("", ""), "an unknown display changes nothing");
    }
}
