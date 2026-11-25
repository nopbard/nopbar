//! What the settings window shows: sections of rows, each bound to one
//! config field. Only settings the dock actually honours are listed.

use crate::config::{Config, CornerAction, DisplayDock, DockCfg, Indicator, Position, ACCENTS, ACCENT_CUSTOM_TEXT, DESKTOPS_MODES, FINISH_CHOICES, INDICATOR_MODES, THEME_MODES, TOPBAR_POSITIONS, TOPBAR_STYLES};
#[cfg(test)]
use crate::effects;
use crate::shortcuts::{self, Kind as SKind, DIGIT_SETS};

pub enum Ctl {
    Toggle { get: fn(&Config) -> bool, set: fn(&mut Config, bool) },
    /// `unit`: text key of the unit after the value ("" = none).
    Slider { get: fn(&Config) -> f32, set: fn(&mut Config, f32), min: f32, max: f32, step: f32, unit: &'static str },
    /// `opts`: text keys of the options.
    Choice { opts: &'static [&'static str], get: fn(&Config) -> usize, set: fn(&mut Config, usize) },
    /// Like Choice, for longer lists: a button showing the current option
    /// that opens the list under it.
    Menu { opts: &'static [&'static str], get: fn(&Config) -> usize, set: fn(&mut Config, usize) },
    /// Does something right away (not a setting). `text`: its text key.
    Button { text: &'static str },
    /// A shortcut from crate::shortcuts (a key recorder, a switch or a
    /// fixed chord, by its kind), with its Reset.
    Shortcut { id: &'static str },
    /// A heading over the rows that follow (not a setting).
    Header,
    /// A closed group ("Advanced", "More options"): the `n` rows after it
    /// show once it is opened (not a setting).
    Fold { n: usize },
}

impl Ctl {
    /// A list of options (a Menu, or a Choice drawn as one when its
    /// options are too wide for the row): (options, get, set).
    #[allow(clippy::type_complexity)]
    pub fn options(&self) -> Option<(&'static [&'static str], fn(&Config) -> usize, fn(&mut Config, usize))> {
        match *self {
            Ctl::Menu { opts, get, set } | Ctl::Choice { opts, get, set } => Some((opts, get, set)),
            _ => None,
        }
    }
}

pub struct Row {
    /// Stable name of the field (sent to the dock with edits).
    pub key: &'static str,
    /// Text keys (crate::i18n) of the label and of the small grey line
    /// under it ("" = none).
    pub label: &'static str,
    pub hint: &'static str,
    pub ctl: Ctl,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Rows,
    CompanionAi,
    /// The top bar: a picture of the bar (topbar_view.rs) over the rows.
    TopBar,
    /// The dock: a map of the displays (displays_view.rs) over the rows;
    /// with a dock on every display the rows set the dock of the display
    /// picked on the map (see `editing`).
    Dock,
    /// The pinned apps list (order, icons, remove).
    Pins,
    /// The app (version, updates and their switch: update_card.rs),
    /// report a problem, project links.
    About,
    /// Every shortcut (crate::shortcuts), grouped under headings.
    Shortcuts,
    /// Input (crate::ime): the memory row, then the per-app rules list.
    Input,
    /// Window management: scrolling through desktops, the hot corners (a
    /// drawing of the screen over their rows).
    Windows,
    /// The clock: its seconds, the focus timer, then the calendar links
    /// (calendar.rs) under the last heading.
    Calendar,
}

/// The sidebar's entries, in order: one page each, or (Dock, Top bar,
/// Keyboard) a few shown as sub-pages under its title; in clusters
/// separated by a little space (Tab::cluster), the way macOS System
/// Settings groups its sidebar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    General,
    Appearance,
    Dock,
    TopBar,
    Launchpad,
    Windows,
    Agents,
    Companion,
    CompanionAi,
    Keyboard,
    About,
}

impl Tab {
    /// Every entry in the sidebar, cluster by cluster: NopBar itself (and
    /// how it looks); what it puts on screen and its helpers; the keyboard
    /// and about.
    pub const ALL: [Tab; 11] = [Tab::General, Tab::Appearance, Tab::Dock, Tab::TopBar, Tab::Launchpad, Tab::Windows,
        Tab::Companion, Tab::CompanionAi, Tab::Agents, Tab::Keyboard, Tab::About];

    pub fn companion_child(self) -> bool {
        matches!(self, Tab::CompanionAi | Tab::Agents)
    }

    /// The cluster it is listed in (a gap in the sidebar between two).
    pub fn cluster(self) -> u8 {
        match self {
            Tab::General | Tab::Appearance => 0,
            Tab::Dock | Tab::TopBar | Tab::Launchpad | Tab::Windows | Tab::Agents | Tab::Companion | Tab::CompanionAi => 1,
            Tab::Keyboard | Tab::About => 2,
        }
    }

    /// Text key of its name in the sidebar (and over its sub-pages).
    pub fn title(self) -> &'static str {
        match self {
            Tab::General => "settings.section.general",
            Tab::Appearance => "settings.section.appearance",
            Tab::Dock => "settings.section.dock",
            Tab::TopBar => "settings.section.topbar",
            Tab::Launchpad => "settings.tab.search",
            Tab::Windows => "settings.tab.windows",
            Tab::Agents => "settings.tab.agents",
            Tab::Companion => "companion.title",
            Tab::CompanionAi => "chat.ai.title",
            Tab::Keyboard => "settings.tab.keyboard",
            Tab::About => "settings.section.about",
        }
    }

    /// Its monochrome glyph in the sidebar.
    pub fn glyph(self) -> crate::glyphs::Glyph {
        use crate::glyphs::Glyph;
        match self {
            Tab::General => Glyph::Gear,
            Tab::Appearance => Glyph::Appearance,
            Tab::Dock => Glyph::Dock,
            Tab::TopBar => Glyph::TopBar,
            Tab::Launchpad => Glyph::Launchpad,
            Tab::Windows => Glyph::Corners,
            Tab::Agents => Glyph::Sparkle,
            Tab::Companion => Glyph::GnomeHat,
            Tab::CompanionAi => Glyph::Sparkle,
            Tab::Keyboard => Glyph::Keyboard,
            Tab::About => Glyph::Info,
        }
    }
}

/// The sidebar's list, laid out from 0: each entry's top, `pitch` apart
/// with `gap` more between clusters; and the list's whole height. Pure:
/// tested (the window scrolls the list when it is taller than its room).
pub fn sidebar_tops(tabs: &[Tab], pitch: f32, gap: f32) -> (Vec<f32>, f32) {
    let mut tops = Vec::with_capacity(tabs.len());
    let mut y = 0.0;
    for (i, t) in tabs.iter().enumerate() {
        if i > 0 && tabs[i - 1].cluster() != t.cluster() {
            y += gap;
        }
        tops.push(y);
        y += pitch;
    }
    (tops, y)
}

pub struct Section {
    /// Text key (the sub-page's name; the page's title when it is alone).
    pub title: &'static str,
    pub tab: Tab,
    /// Text key of the one sentence over the page.
    pub intro: &'static str,
    pub kind: Kind,
    pub rows: Vec<Row>,
}

/// The pages of tab `tab`, by index into `sections`.
pub fn pages_of(sections: &[Section], tab: Tab) -> Vec<usize> {
    sections.iter().enumerate().filter(|(_, s)| s.tab == tab).map(|(i, _)| i).collect()
}

/// The page and row holding setting `key`.
pub fn find_row(sections: &[Section], key: &str) -> Option<(usize, usize)> {
    sections.iter().enumerate().find_map(|(si, s)| s.rows.iter().position(|r| r.key == key).map(|ri| (si, ri)))
}

/// The fold row `ri` is inside (its row index), if any.
pub fn fold_of(rows: &[Row], ri: usize) -> Option<usize> {
    rows.iter().enumerate().take(ri).find_map(|(fi, r)| match r.ctl {
        Ctl::Fold { n } if ri > fi && ri <= fi + n => Some(fi),
        _ => None,
    })
}

const POSITIONS: [Position; 3] = [Position::Bottom, Position::Left, Position::Right];

// ---- the dock of one display ------------------------------------------------

/// The display whose dock the Dock page's rows set: its key and name (None:
/// the main dock). Only the settings window picks one (the map); anywhere
/// else, and inside apply_keys, the rows set the main dock.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Editing {
    pub key: String,
    pub name: String,
}

thread_local! {
    static EDITING: std::cell::RefCell<Option<Editing>> = const { std::cell::RefCell::new(None) };
}

/// Picks the display whose dock the rows set (None: the main dock).
pub fn edit_display(d: Option<Editing>) {
    EDITING.with(|e| *e.borrow_mut() = d);
}

/// The display picked (None: the main dock).
pub fn editing() -> Option<Editing> {
    EDITING.with(|e| e.borrow().clone())
}

/// While alive, the rows set the main dock (apply_keys copies fields by
/// row: never a display's).
struct MainDock(Option<Editing>);

impl MainDock {
    fn enter() -> Self {
        MainDock(EDITING.with(|e| e.borrow_mut().take()))
    }
}

impl Drop for MainDock {
    fn drop(&mut self) {
        let d = self.0.take();
        EDITING.with(|e| *e.borrow_mut() = d);
    }
}

/// The rows that set one display's dock (with "Same as Dock 1" off).
pub const DISPLAY_ROWS: [&str; 4] = ["dock.position", "dock.icon_size", "dock.max_scale", "dock.autohide"];

/// The "Same as Dock 1 (main)" switch.
pub const SAME_AS_MAIN: &str = "dock.same_as_main";

/// The dock settings the rows show: the picked display's.
fn dock_view(c: &Config) -> DockCfg {
    let k = editing().map(|e| e.key);
    c.dock.for_display(k.as_deref())
}

/// Changes the picked display's dock (its own settings, or the main
/// dock's when it has none).
fn dock_set(c: &mut Config, f: impl FnOnce(&mut DockCfg)) {
    let k = editing().map(|e| e.key);
    crate::config::edit_for_display(c, k.as_deref(), |c| f(&mut c.dock));
}

/// The picked display takes the main dock's settings (no entry of its
/// own). The main dock itself always does.
pub fn same_as_main(c: &Config) -> bool {
    editing().is_none_or(|e| !c.dock.per_display.contains_key(&e.key))
}

/// On: the picked display's own settings go (it follows the main dock
/// again). Off: it gets its own, starting from the main dock's now.
fn set_same_as_main(c: &mut Config, on: bool) {
    let Some(e) = editing() else { return };
    if on {
        c.dock.per_display.remove(&e.key);
    } else if !c.dock.per_display.contains_key(&e.key) {
        let own = DisplayDock::copy_of(&c.dock, &e.name);
        c.dock.per_display.insert(e.key, own);
    }
}

/// The picked display has settings of its own (the rows change them).
pub fn editing_own(c: &Config) -> bool {
    editing().is_some_and(|e| c.dock.per_display.contains_key(&e.key))
}
/// The picked dock takes the apps of its own (a dock on every display):
/// its own settings start from the main dock's now (as "Same as Dock 1"
/// turned off); an entry without apps of its own gets the main dock's.
/// False if there is nothing to do (the main dock, or apps of its own).
pub fn customize_pins(c: &mut Config) -> bool {
    let Some(e) = editing() else { return false };
    match c.dock.per_display.get_mut(&e.key) {
        Some(d) if d.pins.is_some() => false,
        Some(d) => {
            d.pins = Some(c.dock.pins.clone());
            true
        }
        None => {
            set_same_as_main(c, false);
            true
        }
    }
}

/// Magnification in four steps (review P2) instead of a 0.1 slider: off,
/// small (1.4, the default, kept as it was), medium, large.
pub const MAGNIFY_STEPS: [f32; 4] = [crate::config::MAGNIFY_OFF, 1.4, 1.6, 2.0];
const MAGNIFY_OPTS: [&str; 4] = ["settings.dock.max_scale.off", "settings.size.small", "settings.size.medium", "settings.size.large"];

/// The step a magnification is shown as: the nearest one (a value from the
/// file between steps, e.g. 1.3 or 2.5), never Off while it magnifies.
pub fn magnify_index(v: f32) -> usize {
    if v <= 1.05 {
        return 0;
    }
    (1..MAGNIFY_STEPS.len()).min_by(|a, b| (MAGNIFY_STEPS[*a] - v).abs().total_cmp(&(MAGNIFY_STEPS[*b] - v).abs())).unwrap_or(1)
}

/// The light / dark choice's options (config::THEME_MODES' order).
const THEME_MODE_OPTS: [&str; 3] = ["settings.theme.mode.auto", "settings.theme.mode.light", "settings.theme.mode.dark"];
/// The accent choice's options (config::ACCENTS' order), then "Custom…"
/// (config::ACCENT_CUSTOM: the settings window asks Windows' color dialog).
const ACCENT_OPTS: [&str; 10] = [ACCENTS[0].1, ACCENTS[1].1, ACCENTS[2].1, ACCENTS[3].1, ACCENTS[4].1, ACCENTS[5].1, ACCENTS[6].1, ACCENTS[7].1, ACCENTS[8].1, ACCENT_CUSTOM_TEXT];
/// The finish choice's options (config::FINISHES' order).
const FINISH_OPTS: [&str; FINISH_CHOICES] = ["settings.theme.finish.flat", "settings.theme.finish.glass"];

/// The text size choice's options (crate::fonts::TEXT_SIZES' order).
const TEXT_SIZE_OPTS: [&str; 3] = ["settings.appearance.text_size.small", "settings.appearance.text_size.default", "settings.appearance.text_size.large"];

/// The installed font families for the Font row, as (family, shown name),
/// recommended ones first (crate::fonts::menu_list); read once.
fn font_families() -> &'static [(String, String)] {
    static LIST: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();
    LIST.get_or_init(|| {
        if cfg!(test) {
            // (An installed Pretendard too: the user's, not the built-in one.)
            vec![("Segoe UI".into(), "Segoe UI".into()), ("Malgun Gothic".into(), "맑은 고딕".into()), ("Pretendard Variable".into(), "Pretendard Variable".into())]
        } else {
            crate::fonts::menu_list(&crate::fonts::families())
        }
    })
}

/// The Font row's options: Pretendard (built in, the default), Windows'
/// font, then every family by its name in the user's language.
pub fn font_opts() -> &'static [&'static str] {
    static OPTS: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    OPTS.get_or_init(|| {
        let mut v = vec!["settings.appearance.font.default", "settings.appearance.font.system"];
        v.extend(font_families().iter().map(|(_, shown)| &*Box::leak(format!("raw:{shown}").into_boxed_str())));
        v
    })
}

/// The family shown: Windows' font for its value, a listed (installed)
/// family by its name, an installed Pretendard too, anything else (none,
/// one not installed) the default, as crate::fonts::pick draws it.
fn font_get(c: &Config) -> usize {
    let f = c.appearance.font.trim();
    if f.eq_ignore_ascii_case(crate::fonts::SYSTEM_CHOICE) {
        return 1;
    }
    font_families().iter().position(|(en, _)| !f.is_empty() && en.eq_ignore_ascii_case(f)).map_or(0, |i| i + 2)
}

fn font_set(c: &mut Config, i: usize) {
    c.appearance.font = match i {
        0 => String::new(),
        1 => crate::fonts::SYSTEM_CHOICE.to_string(),
        i => font_families().get(i - 2).map(|(en, _)| en.clone()).unwrap_or_default(),
    };
}

/// The theme files' choice: the built-in theme, then each valid file in
/// the themes folder (read when the pages are built). Shown only when
/// there is a file (`theme_choices` > 1).
pub fn theme_opts() -> &'static [&'static str] {
    let themes: Vec<(String, String)> = if cfg!(test) { vec![("builtin:graphite".into(), String::new())] } else { listed_themes() };
    static LISTS: std::sync::Mutex<Vec<(Vec<(String, String)>, &'static [&'static str])>> = std::sync::Mutex::new(Vec::new());
    let Ok(mut lists) = LISTS.lock() else { return &["settings.themes.default"] };
    let opts = match lists.iter().find(|(t, _)| *t == themes) {
        Some((_, o)) => *o,
        None => {
            let v: Vec<&'static str> = themes
                .iter()
                .map(|(src, name)| if src.starts_with("builtin:") { "settings.themes.default" } else { &*Box::leak(format!("raw:{name}").into_boxed_str()) })
                .collect();
            let o: &'static [&'static str] = Box::leak(v.into_boxed_slice());
            lists.push((themes.clone(), o));
            o
        }
    };
    if let Ok(mut cur) = SHOWN_THEMES.lock() {
        *cur = themes.into_iter().map(|(s, _)| s).collect();
    }
    opts
}

/// The themes folder's list, read at most every few seconds (the pages are
/// built in the dock too, on every preview).
fn listed_themes() -> Vec<(String, String)> {
    static LAST: std::sync::Mutex<Option<(std::time::Instant, Vec<(String, String)>)>> = std::sync::Mutex::new(None);
    let Ok(mut last) = LAST.lock() else { return super::extra::list_themes() };
    if let Some((at, list)) = last.as_ref() {
        if at.elapsed() < std::time::Duration::from_secs(3) {
            return list.clone();
        }
    }
    let list = super::extra::list_themes();
    *last = Some((std::time::Instant::now(), list.clone()));
    list
}

/// The theme sources of the last list made (theme_opts' order).
static SHOWN_THEMES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// How many themes the choice lists (1: only the built-in one).
pub fn theme_choices() -> usize {
    SHOWN_THEMES.lock().map(|t| t.len()).unwrap_or(1)
}

fn theme_get(c: &Config) -> usize {
    SHOWN_THEMES.lock().ok().and_then(|t| t.iter().position(|s| *s == c.theme.source)).unwrap_or(0)
}

fn theme_set(c: &mut Config, i: usize) {
    if let Some(src) = SHOWN_THEMES.lock().ok().and_then(|t| t.get(i).cloned()) {
        c.theme.source = src;
    }
}
/// The Launchpad pin's row (Dock > In the Dock).
pub const LAUNCHPAD_PIN: &str = "launchpad.pin";

/// Launchpad is in the dock: the main dock's apps.
fn launchpad_pinned(c: &Config) -> bool {
    c.dock.pins.iter().any(|p| crate::launchpad::is_pin(&p.path))
}

/// The Launchpad pin in every dock (its row sits with what every dock
/// shows): on puts it first in each list of apps (the main dock's and
/// each display's own), off takes it out of all of them.
fn set_launchpad_pin(c: &mut Config, on: bool) {
    let lists = std::iter::once(&mut c.dock.pins).chain(c.dock.per_display.values_mut().filter_map(|d| d.pins.as_mut()));
    for list in lists {
        let has = list.iter().any(|p| crate::launchpad::is_pin(&p.path));
        if on && !has {
            list.insert(0, crate::config::PinEntry::new(crate::launchpad::PIN_NAME.into(), crate::launchpad::PIN.into()));
        } else if !on {
            list.retain(|p| !crate::launchpad::is_pin(&p.path));
        }
    }
}

/// Text keys of CornerAction::ALL, in that order.
pub const CORNER_ACTIONS: [&str; 9] = [
    "corners.action.none",
    "corners.action.task_view",
    "corners.action.desktop",
    "corners.action.control_center",
    "corners.action.lock",
    "corners.action.screen_saver",
    "corners.action.notifications",
    "corners.action.next_desktop",
    "corners.action.previous_desktop",
];

fn corner_index(a: CornerAction) -> usize {
    CornerAction::ALL.iter().position(|x| *x == a).unwrap_or(0)
}

fn corner_action(i: usize) -> CornerAction {
    CornerAction::ALL[i.min(CornerAction::ALL.len() - 1)]
}

/// The "Search with" options: NopBar's own, then each launcher that is
/// installed, from what crate::launchers has cached. Never detects or
/// waits here (this runs on the settings window's UI thread, and in the
/// dock through apply_keys): the settings window starts the detection on a
/// helper thread and rebuilds its pages when it is done
/// (WM_LAUNCHERS_FOUND). Tests list them all.
pub fn search_opts() -> &'static [&'static str] {
    use crate::launchers::{Tool, SEARCH_TOOLS};
    let tools: Vec<Tool> = if cfg!(test) {
        SEARCH_TOOLS.to_vec()
    } else {
        let found = crate::launchers::cached();
        SEARCH_TOOLS.iter().copied().filter(|t| found.iter().any(|f| f.tool == *t)).collect()
    };
    // One list per set of tools (a handful at most), kept for good: rows
    // hold them as 'static.
    static LISTS: std::sync::Mutex<Vec<(Vec<Tool>, &'static [&'static str])>> = std::sync::Mutex::new(Vec::new());
    let Ok(mut lists) = LISTS.lock() else { return &["search.with.nopbar"] };
    let opts = match lists.iter().find(|(t, _)| *t == tools) {
        Some((_, o)) => *o,
        None => {
            let mut v = vec!["search.with.nopbar"];
            v.extend(tools.iter().map(|t| t.option()));
            let o: &'static [&'static str] = Box::leak(v.into_boxed_slice());
            lists.push((tools.clone(), o));
            o
        }
    };
    // The list rows built now show: get/set map positions with it.
    if let Ok(mut cur) = SHOWN_TOOLS.lock() {
        *cur = tools;
    }
    opts
}

/// The launchers of the last list made (search_opts' order after NopBar's).
static SHOWN_TOOLS: std::sync::Mutex<Vec<crate::launchers::Tool>> = std::sync::Mutex::new(Vec::new());

fn search_tools() -> Vec<crate::launchers::Tool> {
    SHOWN_TOOLS.lock().map(|t| t.clone()).unwrap_or_default()
}

/// A launcher not listed (gone since it was chosen) shows as NopBar's own:
/// that is what the dock opens then.
fn search_with_get(c: &Config) -> usize {
    search_tools().iter().position(|t| t.id() == c.search.with).map(|i| i + 1).unwrap_or(0)
}

fn search_with_set(c: &mut Config, i: usize) {
    let tools = search_tools();
    c.search.with = match i.checked_sub(1).and_then(|k| tools.get(k)) {
        Some(t) => t.id().to_string(),
        None => crate::launchers::NOPBAR.to_string(),
    };
}

/// The hot corners' other text keys (checked by the tests).
#[cfg(test)]
pub const CORNERS_TEXT: [&str; 1] = ["corners.hint"];

/// Whether row `r` shows an "i" after its label (a tooltip with its
/// description): every description sits behind one - the pages and the
/// first-run flow show labels only. A screen reader hears it as the row's
/// help either way. Folds have none; a group label may (the picked dock's).
pub fn info_hint(r: &Row) -> bool {
    !r.hint.is_empty() && !matches!(r.ctl, Ctl::Fold { .. }) && !subtitled(r)
}

/// Rows whose name alone means nothing to someone new ("Notes": which
/// notes?): their description is drawn under the label as a grey line
/// (like macOS's), and they have no "i". Kept short: every other row
/// keeps its description behind an "i".
pub const SUBTITLED: &[&str] = &["topbar.island"];

/// Pure: is row `r`'s description drawn under its label (SUBTITLED)?
pub fn subtitled(r: &Row) -> bool {
    !r.hint.is_empty() && !matches!(r.ctl, Ctl::Header | Ctl::Fold { .. }) && SUBTITLED.contains(&r.key)
}

/// A row (shorter to write than the struct).
fn row(key: &'static str, label: &'static str, hint: &'static str, ctl: Ctl) -> Row {
    Row { key, label, hint, ctl }
}

fn header(key: &'static str, label: &'static str) -> Row {
    Row { key, label, hint: "", ctl: Ctl::Header }
}

/// "Advanced" over the last `n` rows of a page (`key`: unique, stable).
fn advanced(key: &'static str, n: usize) -> Row {
    Row { key, label: "settings.fold.advanced", hint: "", ctl: Ctl::Fold { n } }
}

macro_rules! toggle {
    ($($f:ident).+) => {
        Ctl::Toggle { get: |c| c.$($f).+, set: |c, v| c.$($f).+ = v }
    };
}

/// Why a switch that is on does not show now (None: it does): the top
/// bar is off, the island is off, no corner has an action yet (the first-run
/// flow's last step, a row's note).
pub fn feature_note(c: &Config, key: &str) -> Option<&'static str> {
    let bar_item = matches!(
        key,
        "topbar.island" | "topbar.show_desktops" | "input.indicator" | "topbar.same_on_all_displays" | "topbar.position" | "topbar.style" | "topbar.clock_seconds"
            | "topbar.show_wifi" | "topbar.show_volume" | "topbar.show_battery"
    );
    match key {
        _ if bar_item && !c.topbar.enabled => Some("features.note.bar_off"),
        "topbar.island_screenshot" if !c.topbar.enabled => Some("features.note.bar_off"),
        "topbar.island_screenshot" if !c.topbar.island => Some("features.note.island_off"),
        _ => None,
    }
}

/// The Dock page's heading over the picked dock's own rows (its name is
/// put in when shown: App::row_label). The display map is drawn right
/// above it.
pub const THIS_DOCK: &str = "group.dock.this";
/// The widgets card's heading (its label and "i" carry what the dock
/// measured: settings::widgets).
pub const WIDGETS_SUMMARY: &str = "group.widgets";
/// The heading over the rows every dock shares.
pub const ALL_DOCKS: &str = "group.dock.items";
/// The Top bar page's Live Island card (its heading).
pub const ISLAND_CARD: &str = "group.topbar.island";
/// Window management's hot corners card (its heading: the screen's
/// drawing goes under it, over the corners' rows).
pub const CORNERS_CARD: &str = "group.windows.corners";

/// Rows that belong to the switch above them: drawn indented under it,
/// and hidden while that switch is off (their value kept). (row key, is
/// the switch it belongs to on?)
pub const SUB_ROWS: &[(&str, fn(&Config) -> bool)] = &[("widgets.inbox", |c| c.widgets.notes),("widgets.notes_per_desktop", |c| c.widgets.notes), ("widgets.notes_reminders", |c| c.widgets.notes), ("widgets.notes_lead", |c| c.widgets.notes), ("widgets.notes_date_hour", |c| c.widgets.notes), ("widgets.device_battery_alerts", |c| c.widgets.device_battery)];

/// Pure: is `key` a row under another switch?
pub fn is_sub_row(key: &str) -> bool {
    SUB_ROWS.iter().any(|(k, _)| *k == key)
}

/// Pure: is `key` a row under a switch that is off now?
pub fn sub_row_off(c: &Config, key: &str) -> bool {
    SUB_ROWS.iter().any(|(k, on)| *k == key && !on(c))
}

pub fn sections() -> Vec<Section> {
    vec![
        // NopBar itself: language, startup, the taskbar.
        Section {
            title: "settings.section.general",
            tab: Tab::General,
            intro: "settings.intro.general",
            kind: Kind::Rows,
            rows: vec![
                language_row(),
                row("start_with_windows", "settings.general.start_with_windows", "settings.general.start_with_windows.hint", toggle!(start_with_windows)),
                // (The first card ends here: rows::row_kind.)
                row("tray_icon", "settings.general.tray_icon", "settings.general.tray_icon.hint", toggle!(tray_icon)),
                // The taskbar, a card of its own without a heading.
                row("hide_taskbar", "settings.general.hide_taskbar", "settings.general.hide_taskbar.hint", toggle!(hide_taskbar)),
                // Quitting gives the taskbar back: right under its switch
                // ("Quit NopBar", [Quit]; what it does in its "i").
                row("action.quit", "settings.general.quit", "settings.general.quit.hint", Ctl::Button { text: "settings.general.quit.button" }),
                // The desktop widgets: the heading (what they cost this PC
                // as a word at its end, the numbers behind its "i":
                // settings::widgets), a switch per kind with what it is
                // and its shortcut under it; screen sharing (every
                // widget's) a card of its own under them.
                Row { key: WIDGETS_SUMMARY, label: "settings.group.widgets", hint: "settings.group.widgets.hint", ctl: Ctl::Header },
                row("widgets.notes", "widgets.notes", "widgets.notes.hint", toggle!(widgets.notes)),
                row("widgets.notes_per_desktop", "widgets.notes_per_desktop", "widgets.notes_per_desktop.hint", toggle!(widgets.notes_per_desktop)),
                row("widgets.notes_reminders", "widgets.notes_reminders", "widgets.notes_reminders.hint", toggle!(widgets.notes_reminders)),
                row("widgets.notes_lead", "widgets.notes_lead", "", Ctl::Choice { opts: &["remind.on_time", "remind.five_before", "remind.ten_before"], get: |c| match c.widgets.notes_lead { 0=>0,5=>1,_=>2 }, set: |c,i| c.widgets.notes_lead=[0,5,10][i.min(2)] }),
                row("widgets.notes_date_hour", "widgets.notes_date_hour", "", Ctl::Choice { opts: &["remind.nine", "remind.ten", "remind.off"], get: |c| match c.widgets.notes_date_hour {9=>0,10=>1,_=>2}, set: |c,i| c.widgets.notes_date_hour=[9,10,0][i.min(2)] }),
                row("widgets.inbox", "widgets.inbox", "widgets.inbox.hint", toggle!(widgets.inbox)),
                row("widgets.device_battery", "widgets.device_battery", "widgets.device_battery.hint", toggle!(widgets.device_battery)),
                row("widgets.device_battery_alerts", "widgets.device_battery_alerts", "widgets.device_battery_alerts.hint", toggle!(widgets.device_battery_alerts)),
                row("widgets.pieces", "widgets.pieces", "widgets.pieces.hint", toggle!(widgets.pieces)),
                row("widgets.hide_from_capture", "widgets.hide_from_capture", "widgets.hide_from_capture.hint", toggle!(widgets.hide_from_capture)),
            ],
        },
        // The theme, the font and the text size (the Look tab's first half
        // before the redesign).
        // The theme, the text, the effects; theme files under Advanced.
        Section { title: "settings.section.appearance", tab: Tab::Appearance, intro: "settings.intro.appearance", kind: Kind::Rows, rows: appearance_rows() },
        Section {
            title: "settings.section.dock",
            tab: Tab::Dock,
            intro: "settings.intro.dock",
            kind: Kind::Dock,
            rows: vec![
                row(
                    "dock.displays",
                    "settings.dock.displays",
                    "settings.dock.displays.hint",
                    Ctl::Choice {
                        opts: &["settings.dock.displays.follow", "settings.dock.displays.main", "settings.dock.displays.every"],
                        get: |c| c.dock.display_mode().index(),
                        set: |c, i| c.dock.set_display_mode(i),
                    },
                ),
                // The map, then the picked dock's own rows under its name
                // (the heading shows only with a dock on every display).
                Row { key: THIS_DOCK, label: "settings.group.this_dock", hint: "settings.group.this_dock.hint", ctl: Ctl::Header },
                // The picked display's dock (shown only for another
                // display than the main one; the rows below are its).
                row(SAME_AS_MAIN, "settings.dock.same_as_main", "settings.dock.same_as_main.hint", Ctl::Toggle { get: same_as_main, set: set_same_as_main }),
                row(
                    "dock.position",
                    "settings.dock.position",
                    "settings.dock.position.hint",
                    Ctl::Choice {
                        opts: &["settings.dock.position.bottom", "settings.dock.position.left", "settings.dock.position.right"],
                        get: |c| POSITIONS.iter().position(|p| *p == dock_view(c).position).unwrap_or(0),
                        set: |c, i| dock_set(c, |d| d.position = POSITIONS[i.min(2)]),
                    },
                ),
                row(
                    "dock.icon_size",
                    "settings.dock.icon_size",
                    "settings.dock.icon_size.hint",
                    Ctl::Slider { get: |c| dock_view(c).icon_size, set: |c, v| dock_set(c, |d| d.icon_size = v), min: 32.0, max: 96.0, step: 2.0, unit: "" },
                ),
                row(
                    "dock.max_scale",
                    "settings.dock.max_scale",
                    "settings.dock.max_scale.hint",
                    Ctl::Choice {
                        opts: &MAGNIFY_OPTS,
                        get: |c| magnify_index(dock_view(c).max_scale),
                        set: |c, i| dock_set(c, |d| d.max_scale = MAGNIFY_STEPS[i.min(MAGNIFY_STEPS.len() - 1)]),
                    },
                ),
                row("dock.autohide", "settings.dock.autohide", "settings.dock.autohide.hint", Ctl::Toggle { get: |c| dock_view(c).autohide, set: |c, v| dock_set(c, |d| d.autohide = v) }),
                header(ALL_DOCKS, "settings.group.in_dock"),
                // The running apps' dot (off: none).
                row("dock.indicator", "settings.dock.indicator", "", Ctl::Choice { opts: &["settings.dock.indicator.bar", "settings.dock.indicator.dot", "settings.dock.max_scale.off"], get: |c| match c.dock.indicator { Indicator::None => 2, Indicator::Dot => 1, _ => 0 }, set: |c, i| c.dock.indicator = match i { 1 => Indicator::Dot, 2 => Indicator::None, _ => Indicator::Bar } }),
                // Launchpad and the Recycle Bin: the items pinned to every
                // dock (Launchpad in each display's own apps too).
                row(LAUNCHPAD_PIN, "settings.general.launchpad_pin", "settings.general.launchpad_pin.hint", Ctl::Toggle { get: launchpad_pinned, set: set_launchpad_pin }),
                row("dock.show_trash", "settings.dock.show_trash", "settings.dock.show_trash.hint", toggle!(dock.show_trash)),
                // The dock's face: out of Advanced (review P2).
                row("dock.squircle", "settings.dock.squircle", "settings.dock.squircle.hint", toggle!(dock.squircle)),
                advanced("fold.dock.advanced", 2),
                row("dock.reserve_work_area", "settings.dock.reserve_work_area", "settings.dock.reserve_work_area.hint", toggle!(dock.reserve_work_area)),
                row("dock.display_apps_only", "settings.dock.app_displays", "", Ctl::Choice {
                    opts: &["settings.dock.app_displays.current", "settings.dock.app_displays.all"],
                    get: |c| usize::from(!c.dock.display_apps_only), set: |c,i| c.dock.display_apps_only = i == 0,
                }),
            ],
        },
        // The dock's apps: a sub-page of the Dock (the display picked on
        // either is picked on both: `editing`).
        Section { title: "settings.section.pins", tab: Tab::Dock, intro: "settings.intro.pins", kind: Kind::Pins, rows: Vec::new() },
        Section {
            title: "settings.section.topbar",
            tab: Tab::TopBar,
            intro: "settings.intro.topbar",
            kind: Kind::TopBar,
            rows: vec![
                row("topbar.enabled", "settings.topbar.enabled", "settings.topbar.enabled.hint", toggle!(topbar.enabled)),
                // Top or bottom edge (every display's bar alike); applies at
                // once, like the bar's other settings.
                row(
                    "topbar.position",
                    "settings.topbar.position",
                    "settings.topbar.position.hint",
                    Ctl::Choice {
                        opts: &["settings.topbar.position.top", "settings.topbar.position.bottom"],
                        get: |c| c.topbar.position(),
                        set: |c, i| c.topbar.position = TOPBAR_POSITIONS[i.min(TOPBAR_POSITIONS.len() - 1)].to_string(),
                    },
                ),
                // What the bar stands on (topbar::look): adaptive glass by default.
                row(
                    "topbar.style",
                    "settings.topbar.style",
                    "settings.topbar.style.hint",
                    Ctl::Menu {
                        opts: &["settings.topbar.style.clear", "settings.topbar.style.frost", "settings.topbar.style.recipe", "settings.topbar.style.solid", "settings.topbar.style.glass"],
                        get: |c| c.topbar.style_for(c.glass()),
                        set: |c, i| c.topbar.style = TOPBAR_STYLES[i.min(TOPBAR_STYLES.len() - 1)].to_string(),
                    },
                ),
                row("topbar.same_on_all_displays", "settings.topbar.same_on_all_displays", "settings.topbar.same_on_all_displays.hint", toggle!(topbar.same_on_all_displays)),
                header("group.topbar.items", "settings.group.on_bar"),
                // What is on the bar, most used first: what is on by
                // default, the features' buttons, then the status readers.
                row("topbar.show_app_menus", "settings.topbar.show_app_menus", "settings.topbar.show_app_menus.hint", toggle!(topbar.show_app_menus)),
                row("topbar.tray", "settings.topbar.tray", "settings.topbar.tray.hint", toggle!(topbar.tray)),
                // Auto (shown when there is more than one) or off.
                row("topbar.show_desktops", "settings.topbar.show_desktops", "settings.topbar.show_desktops.hint", Ctl::Toggle { get: |c| c.topbar.desktops_mode() != 2, set: |c, v| c.topbar.show_desktops = DESKTOPS_MODES[if v { 0 } else { 2 }].to_string() }),
                row("input.indicator", "settings.input.indicator", "settings.input.indicator.hint", Ctl::Toggle { get: |c| c.input.indicator_mode() != 2, set: |c, v| c.input.indicator = INDICATOR_MODES[if v { 0 } else { 2 }].to_string() }),
                // The status icons beside the Quick settings button (the
                // battery on by default).
                row("topbar.show_battery", "settings.topbar.show_battery", "", toggle!(topbar.show_battery)),
                row("topbar.show_wifi", "settings.topbar.show_wifi", "", toggle!(topbar.show_wifi)),
                row("topbar.show_volume", "settings.topbar.show_volume", "", toggle!(topbar.show_volume)),
                // The island, and what depends on it, a card of their own.
                header(ISLAND_CARD, "settings.topbar.island"),
                row("topbar.island", "settings.topbar.island", "settings.topbar.island.hint", toggle!(topbar.island)),
                row("topbar.island_screenshot", "settings.topbar.island_screenshot", "settings.topbar.island_screenshot.hint", toggle!(topbar.island_screenshot)),
                advanced("fold.topbar.advanced", 1),
                row("topbar.height", "settings.topbar.height", "settings.topbar.height.hint", Ctl::Slider { get: |c| c.topbar.height, set: |c, v| c.topbar.height = v, min: 24.0, max: 44.0, step: 2.0, unit: "" }),
            ],
        },
        // The bar's clock: its seconds, the focus timer (crate::focus_timer:
        // started from the clock menu, shown on the island), the clock
        // panel's calendars (their list: a page body of its own, under the
        // last heading).
        Section {
            title: "settings.group.clock",
            tab: Tab::TopBar,
            intro: "settings.intro.clock",
            kind: Kind::Calendar,
            rows: vec![
                header("group.clock.clock", "settings.group.clock"),
                row("topbar.clock_seconds", "settings.topbar.clock_seconds", "settings.topbar.clock_seconds.hint", toggle!(topbar.clock_seconds)),
                // Its switch in the clock menu leads the card.
                Row { key: "group.clock.timer", label: "settings.section.timer", hint: "settings.intro.timer", ctl: Ctl::Header },
                row("focus_timer.enabled", "settings.timer.enabled", "settings.timer.enabled.hint", toggle!(focus_timer.enabled)),
                row(
                    "focus_timer.custom_focus",
                    "settings.timer.custom_focus",
                    "settings.timer.custom_focus.hint",
                    Ctl::Slider { get: |c| c.focus_timer.custom_focus as f32, set: |c, v| c.focus_timer.custom_focus = v.round().clamp(5.0, 180.0) as u32, min: 5.0, max: 180.0, step: 5.0, unit: "timer.unit.min" },
                ),
                row(
                    "focus_timer.custom_break",
                    "settings.timer.custom_break",
                    "settings.timer.custom_break.hint",
                    Ctl::Slider { get: |c| c.focus_timer.custom_break as f32, set: |c, v| c.focus_timer.custom_break = v.round().clamp(1.0, 60.0) as u32, min: 1.0, max: 60.0, step: 1.0, unit: "timer.unit.min" },
                ),
                row("focus_timer.dnd", "settings.timer.dnd", "settings.timer.dnd.hint", toggle!(focus_timer.dnd)),
                // A chime when a phase ends (one switch: the notification
                // it once offered instead is gone).
                row("focus_timer.chime", "settings.timer.chime", "", toggle!(focus_timer.chime)),
                // Its "i": what a calendar link is (the links are the
                // card under it: calendar.rs; the list is the switch).
                Row { key: "group.clock.calendar", label: "settings.section.calendar", hint: "settings.calendar.feeds.hint", ctl: Ctl::Header },
            ],
        },
        // Search and Launchpad (General's Search group before the redesign).
        Section {
            title: "settings.tab.search",
            tab: Tab::Launchpad,
            intro: "settings.intro.search",
            kind: Kind::Rows,
            rows: vec![
                header("group.search.search", "settings.group.search"),
                row("search.with", "search.with", "search.with.hint", Ctl::Menu { opts: search_opts(), get: search_with_get, set: search_with_set }),
                header("group.search.launchpad", "settings.group.launchpad"),
                // The App Library page (off: no Library, new apps on the pages).
                row("launchpad.show_library", "settings.launchpad.library", "settings.launchpad.library.hint", toggle!(launchpad.show_library)),
                // Launchpad's own arrangement (pages, folders) back to name order.
                row("action.launchpad_reset", "settings.launchpad.reset", "settings.launchpad.reset.hint", Ctl::Button { text: "settings.launchpad.reset.button" }),
            ],
        },
        // Window management: scrolling through desktops (the dock and the
        // bar), the hot corners.
        Section {
            title: "settings.tab.windows",
            tab: Tab::Windows,
            intro: "corners.hint",
            kind: Kind::Windows,
            rows: vec![
                // One switch for the dock and the top bar.
                row("dock.wheel_desktops", "settings.dock.wheel_desktops", "", Ctl::Toggle { get: |c| c.dock.wheel_desktops, set: |c, v| (c.dock.wheel_desktops, c.topbar.wheel_desktops) = (v, v) }),
                row("hot_corners.desktop_click", "corners.desktop_click", "", Ctl::Toggle { get: |c| c.hot_corners.desktop_click, set: |c, v| c.hot_corners.desktop_click = v }),
                // Its "i": what a corner does (the drawing of the screen
                // goes right under it: rows.rs).
                Row { key: CORNERS_CARD, label: "settings.tab.corners", hint: "corners.hint", ctl: Ctl::Header },
                row("hot_corners.top_left", "corners.top_left", "", Ctl::Menu { opts: &CORNER_ACTIONS, get: |c| corner_index(c.hot_corners.top_left), set: |c, i| c.hot_corners.top_left = corner_action(i) }),
                row("hot_corners.top_right", "corners.top_right", "", Ctl::Menu { opts: &CORNER_ACTIONS, get: |c| corner_index(c.hot_corners.top_right), set: |c, i| c.hot_corners.top_right = corner_action(i) }),
                row("hot_corners.bottom_left", "corners.bottom_left", "", Ctl::Menu { opts: &CORNER_ACTIONS, get: |c| corner_index(c.hot_corners.bottom_left), set: |c, i| c.hot_corners.bottom_left = corner_action(i) }),
                row("hot_corners.bottom_right", "corners.bottom_right", "", Ctl::Menu { opts: &CORNER_ACTIONS, get: |c| corner_index(c.hot_corners.bottom_right), set: |c, i| c.hot_corners.bottom_right = corner_action(i) }),
            ],
        },
        Section {
            title: "companion.title", tab: Tab::Companion, intro: "", kind: Kind::CompanionAi,
            rows: vec![
                row("companion.enabled", "companion.enabled", "", toggle!(companion.enabled)),
                row("companion.visible", "companion.visible", "", Ctl::Toggle {get:|c|c.companion.visible,set:|c,v|c.companion.set_visible(v)}),
                header("group.companion.appearance", "companion.group.appearance"),
                row("companion.size", "companion.size", "", Ctl::Slider { get:|c|c.companion.size.scale()*100., set:|c,v|c.companion.size=crate::companion::model::Size::new(v/100.), min:28.,max:80.,step:1.,unit:"" }),
                row("companion.animations", "companion.motion", "", toggle!(companion.animations)),
                row("companion.boil", "companion.boil", "companion.boil.hint", toggle!(companion.boil)),
                header("group.companion.activities", "companion.group.activities"),
                row("companion.situations", "companion.situations", "companion.situations.subtitle", toggle!(companion.situations)),
                row("companion.download_notifications", "assistant.download_setting", "", toggle!(companion.download_notifications)),
                row("companion.day_wrap", "assistant.wrap", "", toggle!(companion.day_wrap)),
                row("companion.morning_briefing", "assistant.morning", "", toggle!(companion.morning_briefing)),
                row("action.capture_privacy", "widgets.hide_from_capture", "widgets.hide_from_capture.hint", Ctl::Button { text: "dock.menu.open" }),
                header("group.companion.mouse", "companion.group.mouse"),
                row("companion.ignore_mouse", "companion.ignore_mouse.title", "companion.ignore_mouse.hint", toggle!(companion.ignore_mouse)),
            ],
        },
        Section {
            title: "chat.ai.title", tab: Tab::CompanionAi, intro: "", kind: Kind::CompanionAi,
            rows: vec![
                row("chat.history", "chat.history", "", Ctl::Button{text:"chat.history"}),
                row("chat.keep_history", "chat.keep_history", "", toggle!(chat.keep_history)),
                row("chat.login", "chat.login", "chat.privacy", Ctl::Button{text:"chat.login"}),
                row("chat.add", "chat.add", "", Ctl::Button{text:"chat.add"}),
                row("chat.account", "chat.account", "", Ctl::Button{text:"chat.choose"}),
                row("chat.disconnect", "chat.disconnect", "", Ctl::Button{text:"chat.disconnect"}),
                row("chat.model", "chat.model", "", Ctl::Button{text:"chat.choose"}),
                row("chat.daily_limit", "chat.daily_limit", "", Ctl::Slider {get:|c|c.chat.daily_limit as f32,set:|c,v|c.chat.daily_limit=v.round() as u32,min:0.,max:1000.,step:10.,unit:""}),
            ],
        },
        // The AI agents (Claude Code, Codex): connecting each (NopBar's
        // own hook or notify entry in its settings, nothing else of them
        // touched), and keeping the PC awake.
        Section {
            title: "settings.tab.agents",
            tab: Tab::Agents,
            intro: "settings.intro.agents",
            kind: Kind::Rows,
            rows: vec![
                row("chat.claude_mcp", "settings.agents.claude", "", Ctl::Toggle {
                    get: |c| c.agents.claude || c.chat.claude_mcp,
                    set: |c, on| c.set_agent_notifications(0, on),
                }),
                row("chat.codex_mcp", "settings.agents.codex", "", Ctl::Toggle {
                    get: |c| c.agents.codex || c.chat.codex_mcp,
                    set: |c, on| c.set_agent_notifications(1, on),
                }),
                row("chat.antigravity_mcp", "chat.antigravity_mcp", "", toggle!(chat.antigravity_mcp)),
                header("group.agents.pc", "settings.group.agents.pc"),
                row("agents.keep_awake", "settings.agents.keep_awake", "settings.agents.keep_awake.hint", toggle!(agents.keep_awake),
                ),
            ],
        },
        // The keyboard: every shortcut (the only place one is set), then
        // the input methods.
        Section { title: "settings.section.shortcuts", tab: Tab::Keyboard, intro: "shortcuts.page_hint", kind: Kind::Shortcuts, rows: shortcut_rows() },
        Section {
            title: "settings.section.input",
            tab: Tab::Keyboard,
            intro: "settings.input.page_hint",
            kind: Kind::Input,
            rows: vec![row("input.remember", "settings.input.remember", "settings.input.remember.hint", toggle!(input.remember))],
        },
        Section { title: "settings.section.about", tab: Tab::About, intro: "settings.intro.about", kind: Kind::About, rows: Vec::new() },
    ]
}

/// Fixed keys that need no row (they do what they do everywhere: Ctrl+Z
/// undoes, Esc cancels, Space looks at the selected item).
pub const UNLISTED_SHORTCUTS: [&str; 3] = ["dock_undo", "dock_cancel_drag", shortcuts::QUICK_LOOK];

/// The Shortcuts page: a heading per area, then its shortcuts in registry
/// order. Built from crate::shortcuts::ALL, so a shortcut added there is
/// listed here without any other change (but UNLISTED_SHORTCUTS).
pub fn shortcut_rows() -> Vec<Row> {
    let mut rows = Vec::new();
    for (area, title) in shortcuts::AREAS {
        let mut first = true;
        for s in shortcuts::ALL.iter().filter(|s| s.area == area && !UNLISTED_SHORTCUTS.contains(&s.id)) {
            if first {
                rows.push(Row { key: title, label: title, hint: "", ctl: Ctl::Header });
                first = false;
            }
            let ctl = match s.kind {
                SKind::Digits => Ctl::Menu { opts: &DIGIT_OPTS, get: digits_get, set: digits_set },
                _ => Ctl::Shortcut { id: s.id },
            };
            rows.push(Row { key: shortcuts::row_key(s.id), label: s.label, hint: s.hint, ctl });
        }
    }
    rows
}

/// DIGIT_SETS' text keys (the "dock_apps" row's options).
const DIGIT_OPTS: [&str; 4] = [DIGIT_SETS[0].1, DIGIT_SETS[1].1, DIGIT_SETS[2].1, DIGIT_SETS[3].1];

fn digits_get(c: &Config) -> usize {
    let v = c.shortcut(shortcuts::DOCK_APPS);
    DIGIT_SETS.iter().position(|(d, _)| *d == v).unwrap_or(0)
}

fn digits_set(c: &mut Config, i: usize) {
    c.set_shortcut(shortcuts::DOCK_APPS, DIGIT_SETS[i.min(DIGIT_SETS.len() - 1)].0);
}

/// Is row `r` a shortcut that can be changed (not a heading, not fixed)?
pub fn is_setting(r: &Row) -> bool {
    match r.ctl {
        Ctl::Button { .. } | Ctl::Header | Ctl::Fold { .. } => false,
        Ctl::Shortcut { id } => shortcuts::find(id).is_some_and(|s| s.kind != SKind::Fixed),
        _ => true,
    }
}

/// The Appearance page: the theme (light or dark as pictures, its card
/// alone; then the finish and the accent's colors), the text (font and
/// size), the effects, and under Advanced the theme file and its folder.
pub fn appearance_rows() -> Vec<Row> {
    vec![
        header("group.look.theme", "settings.section.theme"),
        row("theme.mode", "settings.theme.mode", "settings.theme.mode.hint", Ctl::Choice { opts: &THEME_MODE_OPTS, get: |c| c.theme.mode_index(), set: |c, i| c.theme.mode = THEME_MODES[i.min(THEME_MODES.len() - 1)].to_string() }),
        // Flat or Glass: the finish and the blur behind
        // the dock and menus as one choice.
        row("theme.finish", "settings.theme.finish", "", Ctl::Choice { opts: &FINISH_OPTS, get: |c| c.finish_choice(), set: |c, i| c.set_finish_choice(i.min(FINISH_CHOICES - 1)) }),
        // The accent: a row of colors (Automatic first, Custom… last).
        row("theme.accent", "settings.theme.accent", "settings.theme.accent.hint", Ctl::Menu { opts: &ACCENT_OPTS, get: |c| c.theme.accent_index(), set: |c, i| c.theme.set_accent_index(i) }),
        // The UI font and text size (crate::fonts), for every surface.
        header("group.look.text", "settings.group.text"),
        row("appearance.font", "settings.appearance.font", "settings.appearance.font.hint", Ctl::Menu { opts: font_opts(), get: font_get, set: font_set }),
        row(
            "appearance.text_size",
            "settings.appearance.text_size",
            "settings.appearance.text_size.hint",
            Ctl::Choice { opts: &TEXT_SIZE_OPTS, get: |c| c.appearance.text_size_index(), set: |c, i| c.appearance.text_size = crate::fonts::TEXT_SIZES[i.min(2)].to_string() },
        ),
        // The effects (the Effects page's two switches before the
        // structure pass).
        header(EFFECTS_CARD, "settings.section.effects"),
    ]
    .into_iter()
    .chain(effect_rows())
    .chain([
        // Theme files: for whoever makes one (the file shown once there is
        // one in the folder).
        advanced("fold.look.advanced", 2),
        row("theme", "settings.theme.file", "settings.theme.file.hint", Ctl::Menu { opts: theme_opts(), get: theme_get, set: theme_set }),
        row("action.themes_folder", "settings.theme.folder", "settings.theme.folder.hint", Ctl::Button { text: "settings.theme.folder.button" }),
    ])
    .collect()
}

/// Appearance's effects card (its heading).
pub const EFFECTS_CARD: &str = "group.look.effects";

/// "Low-spec mode" (Effects): its row key (only the switch: the effects
/// in force follow from it in crate::effects::resolve).
pub const LOW_SPEC: &str = "effects.low_spec";

/// The effects card (Appearance): Low-spec mode alone (everything at its
/// lightest; nothing is lowered on its own). Every other effect keeps its
/// default (config::DEFAULTED, settle_removed); magnification is the
/// Dock's (its Off), the blur is the Appearance page's finish. The renderer
/// (`effects.render`) is set in the file only.
pub fn effect_rows() -> Vec<Row> {
    vec![row(LOW_SPEC, "effects.low_spec", "effects.low_spec.hint", Ctl::Toggle { get: |c| c.effects.low_spec, set: |c, v| c.set_low_spec(v) })]
}

/// Whether row `key` holds the same value in `a` and `b`.
#[cfg(test)]
pub fn same_value(a: &Config, b: &Config, key: &str) -> bool {
    setting_rows().into_iter().find(|r| r.key == key).is_some_and(|r| match r.ctl {
        Ctl::Toggle { get, .. } => get(a) == get(b),
        Ctl::Slider { get, .. } => get(a) == get(b),
        Ctl::Choice { get, .. } | Ctl::Menu { get, .. } => get(a) == get(b),
        Ctl::Shortcut { id } => a.shortcut(id) == b.shortcut(id),
        Ctl::Button { .. } | Ctl::Header | Ctl::Fold { .. } => true,
    })
}

/// Every page's rows.
fn setting_rows() -> Vec<Row> {
    sections().into_iter().flat_map(|s| s.rows).collect()
}

/// Rows of the given keys, taken from the settings pages, in that order.
fn rows_by_key(keys: &[&str]) -> Vec<Row> {
    let mut all: Vec<Row> = setting_rows();
    keys.iter().filter_map(|k| all.iter().position(|r| r.key == *k).map(|i| all.swap_remove(i))).collect()
}

/// The rows of a first-run step (0: the taskbar, 1: the look). Both also
/// draw cards of their own (welcome.rs).
pub fn welcome_rows(step: usize) -> Vec<Row> {
    match step {
        1 => {
            let mut v = vec![language_row()];
            v.extend(rows_by_key(&["start_with_windows"]));
            v
        }
        _ => Vec::new(),
    }
}

/// (tag, own name) of every catalog, read once.
fn language_names() -> &'static [(&'static str, String)] {
    static NAMES: std::sync::OnceLock<Vec<(&'static str, String)>> = std::sync::OnceLock::new();
    NAMES.get_or_init(crate::i18n::languages)
}

/// The language list's choices: ("auto", "Same as Windows"), then (tag,
/// own name) for every catalog.
#[cfg(test)]
pub fn language_choices() -> Vec<(String, String)> {
    let mut out = vec![("auto".to_string(), crate::i18n::t("language.auto").to_string())];
    out.extend(language_names().iter().map(|(tag, name)| (tag.to_string(), name.clone())));
    out
}

/// The languages as Ctl::Menu options: "language.auto" (a text key), then
/// "lang:<tag>" for every catalog (shown by its own name: opt_text).
pub fn language_opts() -> &'static [&'static str] {
    static OPTS: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    OPTS.get_or_init(|| {
        let mut v = vec!["language.auto"];
        v.extend(language_names().iter().map(|(tag, _)| &*Box::leak(format!("lang:{tag}").into_boxed_str())));
        v
    })
}

/// The text of a list option: a language's own name, the text itself
/// ("raw:60", numbers), or a text key's text.
pub fn opt_text(o: &'static str) -> &'static str {
    if let Some(raw) = o.strip_prefix("raw:") {
        return raw;
    }
    match o.strip_prefix("lang:") {
        Some(tag) => language_names().iter().find(|(t, _)| *t == tag).map(|(_, n)| n.as_str()).unwrap_or("?"),
        None => crate::i18n::t(o),
    }
}

/// The UI language as a list row (General, and the first-run flow's last
/// step).
pub fn language_row() -> Row {
    Row {
        key: "language",
        label: "settings.section.language",
        hint: "settings.general.language.hint",
        ctl: Ctl::Menu {
            opts: language_opts(),
            get: |c| language_opts().iter().position(|o| o.strip_prefix("lang:").unwrap_or("auto") == c.language).unwrap_or(0),
            set: |c, i| c.language = language_opts().get(i).and_then(|o| o.strip_prefix("lang:")).unwrap_or("auto").to_string(),
        },
    }
}

/// What "Reset to defaults" goes back to: a new config with `pins` (a new
/// config's pins: Launchpad, then the taskbar's; config::imported_pins).
pub fn defaults(pins: Vec<crate::config::PinEntry>) -> Config {
    let mut c = Config::default();
    c.dock.pins = pins;
    c
}

/// The settings a page holds (what its "Reset to defaults" puts back).
pub fn page_keys(sec: &Section) -> Vec<&'static str> {
    match sec.kind {
        Kind::CompanionAi | Kind::Rows | Kind::TopBar | Kind::Shortcuts | Kind::Windows => sec.rows.iter().filter(|r| is_setting(r)).map(|r| r.key).collect(),
        // Its rows, and each display's own dock (the switch's field).
        Kind::Dock => {
            let mut v: Vec<&'static str> = sec.rows.iter().filter(|r| is_setting(r) && r.key != SAME_AS_MAIN).map(|r| r.key).collect();
            v.push("dock.per_display");
            v
        }
        Kind::Input => {
            let mut v: Vec<&'static str> = sec.rows.iter().filter(|r| is_setting(r)).map(|r| r.key).collect();
            v.push("input.rules");
            v
        }
        Kind::Calendar => {
            let mut v: Vec<&'static str> = sec.rows.iter().filter(|r| is_setting(r)).map(|r| r.key).collect();
            v.push("calendar.feeds");
            v
        }
        Kind::Pins => vec!["dock.pins", "dock.icon_overrides"],
        // The update card's switch (not a row): checking and installing.
        Kind::About => vec!["updates.auto_check", "updates.auto_install"],
    }
}

/// Puts page `sec`'s settings in `cfg` back to `defaults`. Returns the keys
/// to send, with the list edits it makes ("-pin:", "+pin:", "-icon:").
pub fn reset_page(cfg: &mut Config, defaults: &Config, sec: &Section) -> Vec<String> {
    let mut keys: Vec<String> = page_keys(sec).iter().map(|k| k.to_string()).collect();
    if sec.kind == Kind::Pins {
        let has = |list: &[crate::config::PinEntry], path: &str| list.iter().any(|p| p.path == path);
        // The list the page shows: the picked display's own apps.
        if let Some(k) = editing().map(|e| e.key).filter(|k| cfg.dock.per_display.get(k).is_some_and(|d| d.pins.is_some())) {
            return reset_display_pins(cfg, defaults, &k);
        }
        keys.extend(cfg.dock.pins.iter().filter(|p| !has(&defaults.dock.pins, &p.path)).map(|p| format!("-pin:{}", p.path)));
        keys.extend(defaults.dock.pins.iter().filter(|p| !has(&cfg.dock.pins, &p.path)).map(|p| format!("+pin:{}", p.path)));
        // Icons of apps another display's dock still has stay.
        let elsewhere = |a: &str| cfg.dock.per_display.values().filter_map(|d| d.pins.as_ref()).any(|l| has(l, a));
        keys.extend(cfg.dock.icon_overrides.keys().filter(|a| !defaults.dock.icon_overrides.contains_key(*a) && !elsewhere(a)).map(|a| format!("-icon:{a}")));
    }
    // Each display's dock follows the main one again; their apps stay
    // (the Pinned apps page holds those).
    let kept: Vec<(String, DisplayDock)> = cfg.dock.per_display.iter().filter_map(|(k, d)| d.pins.clone().map(|p| (k.clone(), DisplayDock { name: d.name.clone(), pins: Some(p), ..Default::default() }))).collect();
    apply_keys(defaults, cfg, &keys);
    if sec.kind == Kind::Dock {
        cfg.dock.per_display = kept.into_iter().collect();
    }
    keys
}

/// The Pinned apps page's reset for display `key`'s own list: back to
/// `defaults`' apps; icons of apps no other dock has go. Returns the keys
/// (the per-display map, its "-pin@" / "+pin@" edits, the icons).
fn reset_display_pins(cfg: &mut Config, defaults: &Config, key: &str) -> Vec<String> {
    let has = |list: &[crate::config::PinEntry], path: &str| list.iter().any(|p| p.path == path);
    let cur = cfg.dock.per_display.get(key).and_then(|d| d.pins.clone()).unwrap_or_default();
    let mut keys = vec!["dock.per_display".to_string()];
    keys.extend(cur.iter().filter(|p| !has(&defaults.dock.pins, &p.path)).map(|p| format!("-pin@{key}:{}", p.path)));
    keys.extend(defaults.dock.pins.iter().filter(|p| !has(&cur, &p.path)).map(|p| format!("+pin@{key}:{}", p.path)));
    let elsewhere = |a: &str| has(&cfg.dock.pins, a) || has(&defaults.dock.pins, a) || cfg.dock.per_display.iter().filter(|(k, _)| k.as_str() != key).filter_map(|(_, d)| d.pins.as_ref()).any(|l| has(l, a));
    let gone: Vec<String> = cur.iter().map(|p| p.path.clone()).filter(|a| cfg.dock.icon_overrides.contains_key(a) && !elsewhere(a)).collect();
    if !gone.is_empty() {
        keys.push("dock.icon_overrides".to_string());
        for a in gone {
            cfg.dock.icon_overrides.remove(&a);
            keys.push(format!("-icon:{a}"));
        }
    }
    if let Some(d) = cfg.dock.per_display.get_mut(key) {
        d.pins = Some(defaults.dock.pins.clone());
    }
    keys
}

/// "Reset all settings": every page but the pinned apps (kept: they are
/// the user's apps, not a setting), back to `defaults`. Returns the keys.
pub fn reset_all(cfg: &mut Config, defaults: &Config, sections: &[Section]) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for s in sections.iter().filter(|s| s.kind != Kind::Pins) {
        for k in reset_page(cfg, defaults, s) {
            if !keys.contains(&k) {
                keys.push(k);
            }
        }
    }
    keys
}

/// A search hit: a page, or one of its rows.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Found {
    pub section: usize,
    pub row: Option<usize>,
}

/// How well row `r` matches `q` (None: not at all): by its label, by the
/// other words people use for it ("search.alias.<key>": "monitor",
/// "screen" for the display rows) a little behind, and by its description
/// (two letters or more, never by initials) behind every label. Folds
/// ("Advanced", "More options") are not rows; a heading is found by its
/// name.
fn row_score(q: &crate::search::matching::Query, query_len: usize, r: &Row) -> Option<u32> {
    use crate::search::matching::INITIALS;
    if matches!(r.ctl, Ctl::Fold { .. }) {
        return None;
    }
    let alias = crate::i18n::get(&format!("search.alias.{}", r.key)).and_then(|a| q.score_loose(a)).map(|s| s.saturating_sub(1));
    let label = q.score_loose(crate::i18n::t(r.label)).max(alias);
    if label.is_some() {
        return label;
    }
    if query_len < 2 || r.hint.is_empty() {
        return None;
    }
    let hint = crate::i18n::t(r.hint);
    let n: Vec<char> = crate::search::matching::fold_loose(hint);
    match q.tier(&n) {
        // Below any label (INITIALS is 1 000 000 and up): by tier, then
        // the shorter description.
        Some((tier, _)) if tier != INITIALS => Some(HINT_BASE + tier * 10_000 + (9_999 - n.len().min(9_999) as u32)),
        _ => None,
    }
}

/// Where description matches rank (under every label match).
const HINT_BASE: u32 = 100_000;

/// Rows (by label, alias or description) and pages (by title, or by their
/// tab's name) matching `query` in the language shown: case and accents do
/// not matter, Hangul initials and a syllable still being typed match
/// (search::matching). Best first. The rows under a heading that matches
/// are found too (behind the heading's own page).
pub fn search(sections: &[Section], query: &str) -> Vec<Found> {
    let q = crate::search::matching::Query::loose(query);
    if q.is_empty() {
        return Vec::new();
    }
    let len = query.trim().chars().count();
    let mut out: Vec<(u32, Found)> = Vec::new();
    for (si, s) in sections.iter().enumerate() {
        let first_of_tab = sections.iter().position(|x| x.tab == s.tab) == Some(si);
        let tab = if first_of_tab && s.tab.title() != s.title { q.score_loose(crate::i18n::t(s.tab.title())) } else { None };
        if let Some(score) = q.score_loose(crate::i18n::t(s.title)).max(tab) {
            out.push((score, Found { section: si, row: None }));
        }
        for (ri, r) in s.rows.iter().enumerate() {
            if let Some(score) = row_score(&q, len, r) {
                out.push((score, Found { section: si, row: Some(ri) }));
            }
        }
    }
    // Best first; equal scores keep the pages' order (the sort is stable).
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out.into_iter().map(|(_, f)| f).collect()
}

/// What page `sec` shows while `query` is searched: per row, whether it
/// is drawn. The rows that match, the heading over each (its group), the
/// fold holding one (opened); a heading that matches keeps its whole
/// group; a page whose title matches shows whole. None: nothing on the
/// page matches, or nothing is searched (the page is shown whole). Pure:
/// tested.
pub fn page_filter(sec: &Section, query: &str) -> Option<Vec<bool>> {
    let q = crate::search::matching::Query::loose(query);
    if q.is_empty() || q.score_loose(crate::i18n::t(sec.title)).is_some() || q.score_loose(crate::i18n::t(sec.tab.title())).is_some() {
        return None;
    }
    let len = query.trim().chars().count();
    let n = sec.rows.len();
    let mut show = vec![false; n];
    // The heading each row is under (its index), and the fold.
    let mut heading: Option<usize> = None;
    let mut heading_matches = false;
    for (ri, r) in sec.rows.iter().enumerate() {
        if let Ctl::Header = r.ctl {
            heading = Some(ri);
            heading_matches = q.score_loose(crate::i18n::t(r.label)).is_some();
            if heading_matches {
                show[ri] = true;
            }
            continue;
        }
        let hit = heading_matches || row_score(&q, len, r).is_some();
        if !hit {
            continue;
        }
        show[ri] = true;
        // A row under a switch: with that switch (hidden while it is off,
        // the row is reached through it).
        if is_sub_row(r.key) && ri > 0 {
            show[ri - 1] = true;
        }
        if let Some(h) = heading {
            show[h] = true;
        }
        if let Some(f) = fold_of(&sec.rows, ri) {
            show[f] = true;
        }
    }
    // A fold shown alone (its rows match nothing) is not.
    for (fi, r) in sec.rows.iter().enumerate() {
        if let Ctl::Fold { n: k } = r.ctl {
            if show[fi] && !(fi + 1..=(fi + k).min(n - 1)).any(|i| show[i]) {
                show[fi] = false;
            }
        }
    }
    show.iter().any(|s| *s).then_some(show)
}

/// The slot a pin dragged in the list lands in: the one under the dragged
/// row's middle (`center_y`), for `n` rows `pitch` apart from `top`.
pub fn drop_slot(center_y: f32, top: f32, pitch: f32, n: usize) -> usize {
    if n == 0 || pitch <= 0.0 {
        return 0;
    }
    let s = ((center_y - top) / pitch).floor();
    if s <= 0.0 { 0 } else { (s as usize).min(n - 1) }
}

/// Moves `v[from]` to `to` (the others shift over). False if nothing moved.
pub fn move_item<T>(v: &mut Vec<T>, from: usize, to: usize) -> bool {
    if from == to || from >= v.len() || to >= v.len() {
        return false;
    }
    let x = v.remove(from);
    v.insert(to, x);
    true
}

/// Copies the fields named in `keys` from `from` into `to`.
pub fn apply_keys(from: &Config, to: &mut Config, keys: &[String]) {
    // Fields go by row: the main dock's, whatever display is picked.
    let _main = MainDock::enter();
    // List edits (the pinned apps / appearance pages) are merged into `to`, which may
    // have changed meanwhile (the dock pinned something): "-pin:<path>" and
    // "-icon:<path>" name what the window removed, "+pin:<path>" what it added.
    let removed = |prefix: &str| -> Vec<String> { keys.iter().filter_map(|k| k.strip_prefix(prefix).map(String::from)).collect() };
    for k in keys {
        match k.as_str() {
            "dock.pins" => to.dock.pins = merge_pins(&to.dock.pins, &from.dock.pins, &removed("-pin:"), &removed("+pin:")),
            "dock.icon_overrides" => {
                for (app, file) in &from.dock.icon_overrides {
                    to.dock.icon_overrides.insert(app.clone(), file.clone());
                }
                for app in removed("-icon:") {
                    to.dock.icon_overrides.remove(&app);
                }
            }
            // Each display's own dock: by value, its pins merged like the
            // main dock's ("-pin@<display>:<path>", "+pin@<display>:<path>").
            "dock.per_display" => to.dock.per_display = merge_displays(&to.dock.per_display, &from.dock.per_display, keys),
            // The theme file (its light / dark and accent are rows of their own).
            "theme" => to.theme.source = from.theme.source.clone(),
            // By value: "Custom…" is any color (its index says none).
            "theme.accent" => to.theme.accent = from.theme.accent.clone(),
            // The settings window's size (no row).
            "settings_window" => to.settings_window = from.settings_window,
            "language" => to.language = from.language.clone(),
            // By value: the two processes list the fonts on their own.
            "appearance.font" => to.appearance.font = from.appearance.font.clone(),
            // By value: the settings window and the dock may have found
            // different launchers, so the list positions may differ.
            "search.with" => to.search.with = from.search.with.clone(),
            // The first-run flow ended (Done / Skip).
            "welcome_done" => to.welcome_done = from.welcome_done,
            // The About page's switch (not a row).
            "updates.auto_check" => to.updates.auto_check = from.updates.auto_check,
            "updates.auto_install" => to.updates.auto_install = from.updates.auto_install,
            // The Input page's rules list (not a row).
            "input.rules" => to.input.rules = from.input.rules.clone(),
            // The Calendar page's links (not a row).
            "calendar.feeds" => to.calendar.feeds = from.calendar.feeds.clone(),
            // Launchpad's arrangement ("Reset to alphabetical": emptied).
            "launchpad.layout" => {
                to.launchpad.layout = from.launchpad.layout.clone();
                // (Reset: an empty Home and schema 0 make every app Home again on the next open.)
                to.launchpad.schema = from.launchpad.schema;
            }
            // Safely Remove's extras: their rows are gone, the settings stay
            // (an older build's journal still carries them: apply::old_keys).
            "topbar.tray_hotplug" => to.topbar.tray_hotplug = from.topbar.tray_hotplug,
            "topbar.removable_dot" => to.topbar.removable_dot = from.topbar.removable_dot,
            // Launchpad's clean-up banner (not a row).
            "chat.model" => {to.chat.model = from.chat.model.clone();to.chat.claude_model=from.chat.claude_model.clone();to.chat.antigravity_model=from.chat.antigravity_model.clone();},
            "agents.claude" => to.set_agent_notifications(0, from.agents.claude || from.chat.claude_mcp),
            "agents.codex" => to.set_agent_notifications(1, from.agents.codex || from.chat.codex_mcp),
            "chat.provider" => {to.chat.provider=from.chat.provider;to.chat.claude_connected=from.chat.claude_connected;to.chat.antigravity_connected=from.chat.antigravity_connected;},
            "launchpad.cleanup_dismissed" => {
                to.launchpad.cleanup_dismissed = from.launchpad.cleanup_dismissed
            }
            _ => {}
        }
    }
    // Every other row by its own control (the first-run flow's copies too:
    // the row on the setting's page carries it).
    for r in setting_rows() {
        if !keys.iter().any(|k| k == r.key) || matches!(r.key, "search.with" | "language" | "theme" | "appearance.font" | SAME_AS_MAIN) {
            continue;
        }
        match r.ctl {
            Ctl::Toggle { get, set } => set(to, get(from)),
            Ctl::Slider { get, set, min, max, .. } => set(to, get(from).clamp(min, max)),
            Ctl::Choice { get, set, opts } | Ctl::Menu { get, set, opts } => set(to, get(from).min(opts.len() - 1)),
            Ctl::Shortcut { id } => to.set_shortcut(id, &from.shortcut(id)),
            Ctl::Button { .. } | Ctl::Header | Ctl::Fold { .. } => {}
        }
    }
}

/// The window's per-display docks (`desired`) over the dock's (`base`):
/// settings by value; an entry's pins merged with the dock's own changes
/// to them (merge_pins), by the "-pin@<key>:" / "+pin@<key>:" edits.
pub fn merge_displays(
    base: &std::collections::BTreeMap<String, DisplayDock>,
    desired: &std::collections::BTreeMap<String, DisplayDock>,
    keys: &[String],
) -> std::collections::BTreeMap<String, DisplayDock> {
    let mut out = desired.clone();
    for (k, d) in out.iter_mut() {
        let (Some(mine), Some(theirs)) = (d.pins.as_ref(), base.get(k).and_then(|b| b.pins.as_ref())) else { continue };
        let list = |sign: char| -> Vec<String> { keys.iter().filter_map(|x| x.strip_prefix(&format!("{sign}pin@{k}:")).map(String::from)).collect() };
        d.pins = Some(merge_pins(theirs, mine, &list('-'), &list('+')));
    }
    out
}

/// `base` without `removed`, with the pins that `desired` also lists put in
/// `desired`'s order. Pins only in `base` (pinned meanwhile) keep their place.
/// Pins in `added` (by path) that `desired` lists and `base` lacks are put
/// in after the pin that comes before them in `desired` (first if none).
pub fn merge_pins(base: &[crate::config::PinEntry], desired: &[crate::config::PinEntry], removed: &[String], added: &[String]) -> Vec<crate::config::PinEntry> {
    let mut out: Vec<_> = base.iter().filter(|p| !removed.contains(&p.path)).cloned().collect();
    let rank = |p: &str| desired.iter().position(|d| d.path == p);
    let slots: Vec<usize> = out.iter().enumerate().filter(|(_, p)| rank(&p.path).is_some()).map(|(i, _)| i).collect();
    let mut ordered: Vec<_> = slots.iter().map(|&i| out[i].clone()).collect();
    ordered.sort_by_key(|p| rank(&p.path));
    for (s, p) in slots.into_iter().zip(ordered) {
        out[s] = p;
    }
    for (di, p) in desired.iter().enumerate() {
        if !added.contains(&p.path) || out.iter().any(|o| o.path == p.path) {
            continue;
        }
        let at = desired[..di].iter().rev().find_map(|d| out.iter().position(|o| o.path == d.path)).map(|i| i + 1).unwrap_or(0);
        out.insert(at, p.clone());
    }
    out
}

/// Snaps `v` to the slider's step and range.
pub fn snap(v: f32, min: f32, max: f32, step: f32) -> f32 {
    let s = ((v - min) / step).round() * step + min;
    // Kill float noise (0.1 steps) so the value prints and compares cleanly.
    ((s.clamp(min, max)) * 1000.0).round() / 1000.0
}

/// The text of a unit key ("" stays "").
pub fn unit_text(unit: &str) -> &'static str {
    if unit.is_empty() { "" } else { crate::i18n::t(unit) }
}

pub fn fmt_value(v: f32, step: f32, unit: &str) -> String {
    if step < 1.0 {
        format!("{v:.1}{unit}")
    } else {
        format!("{}{unit}", v.round() as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_round_trips() {
        for s in sections() {
            for r in s.rows {
                let mut c = Config::default();
                // "Same as Dock 1" exists only for another display's dock.
                if r.key == SAME_AS_MAIN {
                    c.dock.set_display_mode(2);
                    edit_display(Some(Editing { key: "GSM5B09-X".into(), name: "LG".into() }));
                } else {
                    edit_display(None);
                }
                match r.ctl {
                    Ctl::Toggle { get, set } => {
                        let v = !get(&c);
                        set(&mut c, v);
                        assert_eq!(get(&c), v, "{}", r.label);
                    }
                    Ctl::Slider { get, set, min, max, .. } => {
                        set(&mut c, max);
                        assert_eq!(get(&c), max, "{}", r.label);
                        set(&mut c, min);
                        assert_eq!(get(&c), min, "{}", r.label);
                    }
                    Ctl::Choice { opts, get, set } | Ctl::Menu { opts, get, set } => {
                        for i in 0..opts.len() {
                            set(&mut c, i);
                            assert_eq!(get(&c), i, "{}", r.label);
                        }
                    }
                    Ctl::Shortcut { id } => {
                        let s = shortcuts::find(id).unwrap();
                        let v = match s.kind {
                            SKind::Chord => "ctrl+alt+shift+f9",
                            SKind::Switch(on) => on,
                            _ => continue,
                        };
                        c.set_shortcut(id, v);
                        assert_eq!(c.shortcut(id), v, "{}", r.label);
                    }
                    Ctl::Button { .. } | Ctl::Header | Ctl::Fold { .. } => {}
                }
            }
        }
        edit_display(None);
    }

    fn page<'a>(secs: &'a [Section], title: &str) -> &'a Section {
        secs.iter().find(|s| s.title == title).unwrap_or_else(|| panic!("no page {title}"))
    }

    #[test]
    fn tray_icon_row_applies_live() {
        // The General page's switch reaches the dock through apply_keys.
        let mut from = Config::default();
        from.tray_icon = false;
        let mut to = Config::default();
        apply_keys(&from, &mut to, &[]);
        assert!(to.tray_icon);
        apply_keys(&from, &mut to, &["tray_icon".to_string()]);
        assert!(!to.tray_icon);
        assert!(page(&sections(), "settings.section.general").rows.iter().any(|r| r.key == "tray_icon"), "listed under General");
    }

    #[test]
    fn only_named_fields_are_copied() {
        let mut from = Config::default();
        from.dock.icon_size = 80.0;
        from.dock.autohide = true;
        from.dock.pins = vec![crate::config::PinEntry::new("x".into(), "y".into())];
        let mut to = Config::default();
        apply_keys(&from, &mut to, &["dock.icon_size".to_string()]);
        assert_eq!(to.dock.icon_size, 80.0);
        assert!(!to.dock.autohide, "untouched fields stay");
        assert!(to.dock.pins.is_empty(), "pins only when named");
    }

    #[test]
    fn pin_edits_merge_with_changes_made_meanwhile() {
        let pin = |p: &str| crate::config::PinEntry::new(p.into(), p.into());
        // Window: swapped A and B, removed D. Dock meanwhile pinned C.
        let base = vec![pin("A"), pin("B"), pin("C"), pin("D")];
        let desired = vec![pin("B"), pin("A")];
        let out = merge_pins(&base, &desired, &["D".to_string()], &[]);
        let names: Vec<_> = out.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(names, ["B", "A", "C"]);
        // Through apply_keys.
        let mut from = Config::default();
        from.dock.pins = desired;
        let mut to = Config::default();
        to.dock.pins = base;
        apply_keys(&from, &mut to, &["dock.pins".to_string(), "-pin:D".to_string()]);
        assert_eq!(to.dock.pins.len(), 3);
    }

    #[test]
    fn keys_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for s in sections() {
            for r in s.rows {
                assert!(seen.insert(r.key), "{}", r.key);
            }
        }
    }

    #[test]
    fn every_text_key_exists() {
        let has = |k: &str| assert!(crate::i18n::get(k).is_some(), "missing text key {k}");
        for tab in Tab::ALL {
            has(tab.title());
        }
        has("settings.features.open");
        for s in sections() {
            has(s.title);
            if !s.intro.is_empty() { has(s.intro); }
            for r in s.rows {
                has(r.label);
                if !r.hint.is_empty() {
                    has(r.hint);
                }
                match r.ctl {
                    Ctl::Slider { unit, .. } if !unit.is_empty() => has(unit),
                    Ctl::Choice { opts, .. } | Ctl::Menu { opts, .. } => opts.iter().filter(|o| !o.starts_with("raw:") && !o.starts_with("lang:")).for_each(|o| has(o)),
                    Ctl::Button { text } => has(text),
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn launchpad_pin_toggle() {
        let pin = |p: &str| crate::config::PinEntry::new(p.into(), p.into());
        let mut c = Config::default();
        c.dock.pins = vec![pin("a.lnk"), pin("b.lnk")];
        set_launchpad_pin(&mut c, true);
        assert_eq!(c.dock.pins[0].path, crate::launchpad::PIN, "first");
        set_launchpad_pin(&mut c, true);
        assert_eq!(c.dock.pins.len(), 3, "once");
        // Sent to the dock: only the pin changes, the rest stays as it is there.
        let mut dock = Config::default();
        dock.dock.pins = vec![pin("b.lnk"), pin("c.lnk")];
        apply_keys(&c, &mut dock, &["launchpad.pin".to_string()]);
        assert_eq!(dock.dock.pins.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(), [crate::launchpad::PIN, "b.lnk", "c.lnk"]);
        set_launchpad_pin(&mut dock, false);
        assert_eq!(dock.dock.pins.len(), 2);
        // Every dock: each display's own apps too (a display that follows
        // the main dock has none of its own to change).
        let mut c = every();
        c.dock.pins = vec![pin("a.lnk")];
        c.dock.per_display.insert(MON.into(), DisplayDock { pins: Some(vec![pin("b.lnk")]), ..Default::default() });
        c.dock.per_display.insert("OTHER".into(), DisplayDock { autohide: Some(true), ..Default::default() });
        set_launchpad_pin(&mut c, true);
        assert_eq!(paths(&c), [crate::launchpad::PIN, "a.lnk"]);
        assert_eq!(c.dock.per_display[MON].pins.as_ref().map(|l| l[0].path.as_str()), Some(crate::launchpad::PIN));
        assert!(c.dock.per_display["OTHER"].pins.is_none());
        // Sent to the dock by its key: there too.
        let mut dock = every();
        dock.dock.per_display.insert(MON.into(), DisplayDock { pins: Some(vec![pin("b.lnk"), pin("c.lnk")]), ..Default::default() });
        apply_keys(&c, &mut dock, &[LAUNCHPAD_PIN.to_string()]);
        assert_eq!(dock.dock.per_display[MON].pins.as_ref().unwrap().len(), 3);
        set_launchpad_pin(&mut c, false);
        assert!(!c.dock.per_display[MON].pins.as_ref().unwrap().iter().any(|p| crate::launchpad::is_pin(&p.path)));
        assert_eq!(paths(&c), ["a.lnk"]);
    }

    #[test]
    fn corner_rows_cover_every_action_and_copy_alone() {
        assert_eq!(CORNER_ACTIONS.len(), CornerAction::ALL.len());
        for (k, a) in CORNER_ACTIONS.iter().zip(CornerAction::ALL) {
            assert_eq!(*k, format!("corners.action.{}", a.name()));
        }
        let sec = sections().into_iter().find(|s| s.kind == Kind::Windows).expect("Window management");
        let corners: Vec<&str> = sec.rows.iter().skip_while(|r| r.key != CORNERS_CARD).skip(1).take_while(|r| !matches!(r.ctl, Ctl::Header)).map(|r| r.key).collect();
        assert_eq!(corners, ["hot_corners.top_left", "hot_corners.top_right", "hot_corners.bottom_left", "hot_corners.bottom_right"], "the four corners under their heading (no switch for all of them)");
        let mut from = Config::default();
        from.hot_corners.top_left = CornerAction::TaskView;
        from.hot_corners.bottom_right = CornerAction::Lock;
        from.dock.autohide = true;
        let mut to = Config::default();
        apply_keys(&from, &mut to, &["hot_corners.bottom_right".to_string()]);
        assert_eq!(to.hot_corners.bottom_right, CornerAction::Lock);
        assert_eq!(to.hot_corners.top_left, CornerAction::None, "not named: untouched");
        assert!(!to.dock.autohide);
        assert!(!same_value(&from, &to, "hot_corners.top_left"));
        assert!(same_value(&from, &to, "hot_corners.bottom_right"));
    }

    #[test]
    fn corners_text_keys_exist() {
        CORNERS_TEXT.iter().for_each(|k| assert!(crate::i18n::get(k).is_some(), "missing text key {k}"));
    }

    #[test]
    fn update_switch_is_copied_alone() {
        let mut from = Config::default();
        from.updates.auto_check = false;
        from.dock.autohide = true;
        let mut to = Config::default();
        apply_keys(&from, &mut to, &["updates.auto_check".to_string()]);
        assert!(!to.updates.auto_check);
        assert!(!to.dock.autohide);
        from.updates.auto_install = false;
        apply_keys(&from, &mut to, &["updates.auto_install".to_string()]);
        assert!(!to.updates.auto_install);
    }

    #[test]
    fn language_is_copied() {
        let mut from = Config::default();
        from.language = "ko".into();
        let mut to = Config::default();
        apply_keys(&from, &mut to, &["language".to_string()]);
        assert_eq!(to.language, "ko");
    }

    fn pin(p: &str) -> crate::config::PinEntry {
        crate::config::PinEntry::new(p.into(), p.into())
    }

    fn paths(c: &Config) -> Vec<&str> {
        c.dock.pins.iter().map(|p| p.path.as_str()).collect()
    }

    #[test]
    fn welcome_steps_have_their_rows() {
        let keys = |s: usize| welcome_rows(s).into_iter().map(|r| r.key).collect::<Vec<_>>();
        assert!(keys(0).is_empty(), "the taskbar step is two cards");
        assert_eq!(keys(1), ["language", "start_with_windows"]);
        assert!(keys(2).is_empty(), "two steps only");
        let has = |k: &str| assert!(crate::i18n::get(k).is_some(), "missing text key {k}");
        for r in welcome_rows(1) {
            has(r.label);
        }
        // In the order of General's first card (audit 3.4).
        let secs = sections();
        let general: Vec<&str> = page(&secs, "settings.section.general").rows.iter().map(|r| r.key).collect();
        let at = |k: &str| general.iter().position(|x| *x == k).unwrap();
        assert!(keys(1).windows(2).all(|w| at(w[0]) < at(w[1])), "{:?}", keys(1));
        assert!(keys(1).iter().all(|k| at(k) < at("tray_icon") + 1), "all in the first card");
        // Start with Windows is on by default.
        assert!(Config::default().start_with_windows);
    }

    #[test]
    fn language_row_round_trips_and_defaults_to_windows() {
        let row = language_row();
        let Ctl::Menu { opts, get, set } = row.ctl else { panic!("a list") };
        let mut c = Config::default();
        assert_eq!(c.language, "auto");
        assert_eq!(get(&c), 0, "Same as Windows");
        assert_eq!(opt_text(opts[0]), crate::i18n::t("language.auto"));
        let ko = opts.iter().position(|o| *o == "lang:ko").expect("Korean listed");
        set(&mut c, ko);
        assert_eq!(c.language, "ko");
        assert_eq!(get(&c), ko);
        assert_eq!(opt_text("lang:ko"), "한국어");
        set(&mut c, 0);
        assert_eq!(c.language, "auto");
        assert_eq!(opts.len(), language_choices().len());
        // Sent with its key, the language is copied.
        let mut to = Config::default();
        set(&mut c, ko);
        apply_keys(&c, &mut to, &["language".to_string()]);
        assert_eq!(to.language, "ko");
    }

    #[test]
    fn search_with_is_copied_by_value_and_falls_back() {
        // Every launcher round-trips through the list (tests list them all).
        let secs = sections();
        let (si, ri) = find_row(&secs, "search.with").expect("a Search with row");
        assert_eq!(secs[si].tab, Tab::Launchpad, "the Search & apps page");
        let Ctl::Menu { opts, get, set } = secs[si].rows[ri].ctl else { panic!("a list") };
        assert_eq!(opts.len(), 1 + crate::launchers::SEARCH_TOOLS.len());
        let mut c = Config::default();
        assert_eq!(c.search.with, "nopbar");
        assert_eq!(get(&c), 0);
        set(&mut c, 3);
        assert_eq!(c.search.with, crate::launchers::SEARCH_TOOLS[2].id());
        // A launcher not listed shows as NopBar's own (what the dock opens).
        c.search.with = "gone".into();
        assert_eq!(get(&c), 0);
        // Sent by value, not by list position (the dock's list may differ).
        let mut from = Config::default();
        from.search.with = "everything".into();
        from.dock.autohide = true;
        let mut to = Config::default();
        apply_keys(&from, &mut to, &["search.with".to_string()]);
        assert_eq!(to.search.with, "everything");
        assert!(!to.dock.autohide);
    }

    #[test]
    fn the_per_desktop_row_belongs_to_the_notes() {
        let secs = sections();
        let general = page(&secs, "settings.section.general");
        let at = |k: &str| general.rows.iter().position(|r| r.key == k).unwrap();
        let mut c = Config::default();
        assert!(!sub_row_off(&c, "widgets.notes"));
        assert!(!is_sub_row("widgets.notes"));
        // Notes per desktop: under the notes' switch.
        assert_eq!(at("widgets.notes_per_desktop"), at("widgets.notes") + 1);
        assert!(is_sub_row("widgets.notes_per_desktop") && !sub_row_off(&c, "widgets.notes_per_desktop"));
        c.widgets.notes = false;
        assert!(sub_row_off(&c, "widgets.notes_per_desktop"), "the notes off: hidden");
        // The search finds a row under a switch with that switch.
        let _l = crate::i18n::test_lock("en");
        let f = page_filter(general, crate::i18n::t("widgets.notes_per_desktop")).expect("found");
        assert!(f[at("widgets.notes_per_desktop")] && f[at("widgets.notes")], "with its switch");
    }

    #[test]
    fn welcome_done_is_sent_by_its_key() {
        let mut from = Config::default();
        from.welcome_done = true;
        from.dock.autohide = true;
        let mut to = Config::default();
        apply_keys(&from, &mut to, &["welcome_done".to_string()]);
        assert!(to.welcome_done);
        assert!(!to.dock.autohide);
    }

    #[test]
    fn each_page_resets_only_its_keys() {
        let secs = sections();
        let by_kind = |k: Kind| secs.iter().filter(move |s| s.kind == k);
        let general = page(&secs, "settings.section.general");
        assert_eq!(
            page_keys(general),
            ["language", "start_with_windows", "tray_icon", "hide_taskbar", "widgets.notes", "widgets.notes_per_desktop", "widgets.notes_reminders", "widgets.notes_lead", "widgets.notes_date_hour", "widgets.inbox", "widgets.device_battery", "widgets.device_battery_alerts", "widgets.pieces", "widgets.hide_from_capture"],
            "the buttons are not settings"
        );
        assert_eq!(page_keys(page(&secs, "settings.tab.search")), ["search.with", "launchpad.show_library"]);
        assert_eq!(page_keys(by_kind(Kind::Pins).next().unwrap()), ["dock.pins", "dock.icon_overrides"]);
        // Appearance: the theme's three, the text, the effects, the theme
        // file (Advanced).
        assert_eq!(page_keys(page(&secs, "settings.section.appearance")), ["theme.mode", "theme.finish", "theme.accent", "appearance.font", "appearance.text_size", LOW_SPEC, "theme"]);
        // About holds the updates' switch.
        assert_eq!(page_keys(by_kind(Kind::About).next().unwrap()), ["updates.auto_check", "updates.auto_install"]);
        // Window management: scrolling through desktops and the corners.
        assert_eq!(page_keys(by_kind(Kind::Windows).next().unwrap()), ["dock.wheel_desktops", "hot_corners.desktop_click", "hot_corners.top_left", "hot_corners.top_right", "hot_corners.bottom_left", "hot_corners.bottom_right"]);
        // No key belongs to two pages.
        let mut seen = std::collections::HashSet::new();
        for s in &secs {
            for k in page_keys(s) {
                assert!(seen.insert(k), "{k} on two pages");
            }
        }

        // Resetting the Dock page leaves the others alone.
        let defaults = defaults(vec![pin(crate::launchpad::PIN)]);
        let mut c = Config::default();
        c.dock.icon_size = 80.0;
        c.dock.autohide = true;
        c.topbar.height = 40.0;
        c.hide_taskbar = false;
        c.language = "ko".into();
        let dock = secs.iter().find(|s| s.title == "settings.section.dock").unwrap();
        let keys = reset_page(&mut c, &defaults, dock);
        assert_eq!(c.dock.icon_size, defaults.dock.icon_size);
        assert!(!c.dock.autohide);
        assert_eq!(c.topbar.height, 40.0, "the Top bar page's");
        assert!(!c.hide_taskbar, "the General page's");
        assert_eq!(c.language, "ko");
        assert!(keys.iter().all(|k| page_keys(dock).contains(&k.as_str())), "{keys:?}");

        // The General page: its own switches and the language.
        reset_page(&mut c, &defaults, general);
        assert!(c.hide_taskbar && c.start_with_windows);
        assert_eq!(c.language, "auto");
        // ... and the Dock page puts Launchpad back in the dock.
        c.dock.pins = vec![pin("a.lnk")];
        reset_page(&mut c, &defaults, dock);
        assert_eq!(paths(&c), [crate::launchpad::PIN, "a.lnk"]);
    }

    #[test]
    fn resetting_pins_puts_back_the_first_run_list() {
        let secs = sections();
        let pins_page = secs.iter().find(|s| s.kind == Kind::Pins).unwrap();
        let defaults = defaults(vec![pin("L"), pin("edge.lnk"), pin("files.lnk")]);
        let mut c = Config::default();
        c.dock.pins = vec![pin("files.lnk"), pin("mine.exe"), pin("L")];
        c.dock.icon_overrides.insert("files.lnk".into(), "icons\\x.png".into());
        let keys = reset_page(&mut c, &defaults, pins_page);
        assert_eq!(paths(&c), ["L", "edge.lnk", "files.lnk"]);
        assert!(c.dock.icon_overrides.is_empty());
        for k in ["dock.pins", "dock.icon_overrides", "-pin:mine.exe", "+pin:edge.lnk", "-icon:files.lnk"] {
            assert!(keys.iter().any(|x| x == k), "{k} in {keys:?}");
        }
        // The dock meanwhile pinned "new.exe": it stays, the rest is the reset.
        let mut dock = Config::default();
        dock.dock.pins = vec![pin("files.lnk"), pin("mine.exe"), pin("L"), pin("new.exe")];
        apply_keys(&c, &mut dock, &keys);
        assert_eq!(paths(&dock), ["L", "edge.lnk", "files.lnk", "new.exe"]);
    }

    #[test]
    fn reset_all_keeps_the_pins() {
        let secs = sections();
        let defaults = defaults(vec![pin(crate::launchpad::PIN)]);
        let mut c = Config::default();
        c.dock.pins = vec![pin(crate::launchpad::PIN), pin("mine.exe")];
        c.dock.icon_overrides.insert("mine.exe".into(), "icons\\m.png".into());
        c.dock.position = Position::Left;
        c.topbar.enabled = false;
        c.theme.source = "file:themes\\x.json".into();
        c.language = "de".into();
        c.updates.auto_check = false;
        c.hot_corners.top_left = CornerAction::Lock;
        c.welcome_done = true;
        let keys = reset_all(&mut c, &defaults, &secs);
        assert_eq!(paths(&c), [crate::launchpad::PIN, "mine.exe"]);
        assert_eq!(c.dock.icon_overrides.len(), 1);
        assert_eq!(c.dock.position, Position::Bottom);
        assert!(c.topbar.enabled);
        assert_eq!(c.theme.source, "builtin:graphite");
        assert_eq!(c.language, "auto");
        assert!(c.updates.auto_check);
        assert_eq!(c.hot_corners.top_left, CornerAction::None);
        assert!(c.welcome_done, "not a setting");
        assert!(!keys.iter().any(|k| k == "dock.pins" || k == "dock.icon_overrides" || k.starts_with('-') || k.starts_with('+')), "{keys:?}");
        assert!(keys.iter().any(|k| k == "launchpad.pin"));
    }

    #[test]
    fn shortcuts_page_lists_the_whole_registry() {
        let secs = sections();
        let page = secs.iter().find(|s| s.kind == Kind::Shortcuts).expect("a Shortcuts page");
        for s in shortcuts::ALL {
            let listed = page.rows.iter().any(|r| r.key == shortcuts::row_key(s.id));
            assert_eq!(listed, !UNLISTED_SHORTCUTS.contains(&s.id), "{}", s.id);
        }
        for id in UNLISTED_SHORTCUTS {
            assert_eq!(shortcuts::find(id).map(|s| s.kind), Some(SKind::Fixed), "{id}: fixed, so nothing is lost");
        }
        // Headings first, one per area that has shortcuts.
        assert!(matches!(page.rows[0].ctl, Ctl::Header));
        let heads = page.rows.iter().filter(|r| matches!(r.ctl, Ctl::Header)).count();
        let areas = shortcuts::AREAS.iter().filter(|(a, _)| shortcuts::ALL.iter().any(|s| s.area == *a && !UNLISTED_SHORTCUTS.contains(&s.id))).count();
        assert_eq!(heads, areas);
        // Its reset puts back only real settings (no headings, no fixed keys).
        let keys = page_keys(page);
        assert!(keys.contains(&"shortcuts.search") && keys.contains(&"shortcuts.dock_apps"));
        assert!(!keys.iter().any(|k| k.starts_with("shortcuts.area") || *k == "shortcuts.dock_undo"), "{keys:?}");
        // Edits travel by their key alone.
        let mut from = Config::default();
        from.set_shortcut("search", "ctrl+alt+k");
        from.set_shortcut("dock_apps", "alt");
        from.set_shortcut("launchpad", "alt+f1");
        let mut to = Config::default();
        apply_keys(&from, &mut to, &["shortcuts.search".to_string(), "shortcuts.dock_apps".to_string()]);
        assert_eq!(to.shortcut("search"), "ctrl+alt+k");
        assert_eq!(to.shortcut("dock_apps"), "alt");
        assert_eq!(to.shortcut("launchpad"), "none", "not named: untouched");
        // Reset all shortcuts.
        let mut c = from.clone();
        reset_page(&mut c, &defaults(Vec::new()), page);
        assert_eq!(c.shortcuts, Config::default().shortcuts);
    }

    #[test]
    fn effects_card_is_low_spec_mode_alone() {
        let secs = sections();
        let page = page(&secs, "settings.section.appearance");
        let keys: Vec<&str> = page.rows.iter().skip_while(|r| r.key != EFFECTS_CARD).skip(1).take_while(|r| !matches!(r.ctl, Ctl::Header | Ctl::Fold { .. })).map(|r| r.key).collect();
        assert_eq!(keys, [LOW_SPEC], "one switch: no Auto, no Reduce motion, no presets, no per-effect rows");
        assert!(sections().iter().all(|s| s.title != "settings.section.effects"), "no page of its own");
        let row = page.rows.iter().find(|r| r.key == LOW_SPEC).unwrap();
        assert_eq!(row.hint, "effects.low_spec.hint", "said behind the row's i, nowhere else");
        let Ctl::Toggle { get, set } = row.ctl else { panic!("a switch") };
        // Off by default; on: the lightest, every motion item off.
        let mut c = Config::default();
        assert!(!get(&c));
        set(&mut c, true);
        assert!(get(&c));
        let r = effects::resolve(&c, effects::Env { animations: true, high_contrast: false });
        assert_eq!(r.items, effects::preset(effects::Level::Low));
        assert_eq!(crate::motion::mode(r.motion, r.items.small), crate::motion::Mode::Reduced);
        // Sent to the dock by its row key: every field it sets lands.
        let mut to = Config::default();
        apply_keys(&c, &mut to, &[LOW_SPEC.to_string()]);
        assert_eq!(to.effects, c.effects);
        assert_eq!(to.animation, c.animation);
        // Off: back to a new install's.
        set(&mut c, false);
        assert_eq!(c, Config::default());
    }

    /// Rows made one: scrolling on the dock and the bar, the timer's chime
    /// and notification; choices made switches (a dot or none, Auto or off).
    #[test]
    fn companion_enable_is_searchable_and_does_not_overwrite_visibility() {
        let _lang = crate::i18n::test_lock("en");
        let secs = sections();
        assert!(search(&secs, "use companion").iter().any(|f| f.row.is_some_and(|r| secs[f.section].rows[r].key == "companion.enabled")));
        let mut from = Config::default();
        from.companion.enabled = false;
        let mut to = Config::default();
        to.companion.visible = true;
        apply_keys(&from, &mut to, &["companion.enabled".into()]);
        assert!(!to.companion.enabled);
        assert!(to.companion.visible);
    }

    #[test]
    fn merged_rows_set_every_field_they_stand_for() {
        let secs = sections();
        assert!(find_row(&secs, "topbar.wheel_desktops").is_none() && find_row(&secs, "focus_timer.toast").is_none());
        let Ctl::Toggle { get, set } = row_of("dock.wheel_desktops").ctl else { panic!("a switch") };
        let mut c = Config::default();
        set(&mut c, true);
        assert!(get(&c) && c.dock.wheel_desktops && c.topbar.wheel_desktops);
        let mut to = Config::default();
        apply_keys(&c, &mut to, &["dock.wheel_desktops".to_string()]);
        assert!(to.dock.wheel_desktops && to.topbar.wheel_desktops, "one key carries both");
        for (k, off_is) in [("topbar.show_desktops", "never"), ("input.indicator", "never")] {
            let Ctl::Toggle { get, set } = row_of(k).ctl else { panic!("{k}: a switch") };
            let mut c = Config::default();
            assert!(get(&c), "{k}: on by default");
            set(&mut c, false);
            let v = serde_json::to_value(&c).unwrap();
            let (sec, key) = k.split_once('.').unwrap();
            assert_eq!(v[sec][key].as_str().map(str::to_ascii_lowercase).as_deref(), Some(off_is), "{k}");
        }
    }

    /// The finish and the blur are one control (Appearance): Flat
    /// (opaque) or Glass (blurred; the default). The owner's two since
    /// 2026-11-11.
    #[test]
    fn finish_and_blur_are_one_choice() {
        let row = appearance_rows().into_iter().find(|r| r.key == "theme.finish").unwrap();
        let Ctl::Choice { opts, get, set } = row.ctl else { panic!("a choice") };
        assert_eq!(opts, ["settings.theme.finish.flat", "settings.theme.finish.glass"], "Flat and Glass");
        let mut c = Config::default();
        assert_eq!(get(&c), 1, "Glass by default");
        assert!(effects::items_of(&c).blur, "a glass blurs");
        for i in 1..2 {
            set(&mut c, 0);
            assert!(c.theme.finish_index() == 0 && !effects::items_of(&c).blur);
            c.effects.blur = false;
            set(&mut c, i);
            assert!(c.effects.blur && c.theme.finish_index() == i && effects::items_of(&c).blur);
        }
        set(&mut c, 0);
        assert!(c.theme.finish_index() == 0 && !effects::items_of(&c).blur);
        // Travels as one key.
        set(&mut c, 1);
        let mut to = Config::default();
        to.effects.blur = false;
        apply_keys(&c, &mut to, &["theme.finish".to_string()]);
        assert_eq!((to.effects.blur, to.theme.finish_index()), (true, 1));
        assert!(!effect_rows().iter().any(|r| r.key == "effects.blur"), "not on the Effects page any more");
    }

    #[test]
    fn notification_switches_apply_both_settings_and_keep_chat_connection() {
        let secs = sections();
        for (key, provider) in [("chat.claude_mcp", 0), ("chat.codex_mcp", 1), ("chat.antigravity_mcp", 2)] {
            let (si, ri) = find_row(&secs, key).unwrap();
            let Ctl::Toggle { get, set } = secs[si].rows[ri].ctl else { panic!("toggle") };
            let mut c = Config::default();
            c.chat.activate(crate::companion::chat::state::Provider::ClaudeCode);
            for on in [true, false] {
                set(&mut c, on);
                assert_eq!(get(&c), on);
                let mut applied = Config::default();
                apply_keys(&c, &mut applied, &[key.into()]);
                match provider {
                    0 => assert_eq!((applied.agents.claude, applied.chat.claude_mcp), (on, on)),
                    1 => assert_eq!((applied.agents.codex, applied.chat.codex_mcp), (on, on)),
                    _ => assert_eq!(applied.chat.antigravity_mcp, on),
                }
                assert!(c.chat.claude_connected);
            }
        }
    }

    #[test]
    fn agent_notifications_are_companion_children_and_searchable() {
        let _lang = crate::i18n::test_lock("ko");
        let i = Tab::ALL.iter().position(|t| *t == Tab::Companion).unwrap();
        assert_eq!(&Tab::ALL[i + 1..i + 3], &[Tab::CompanionAi, Tab::Agents]);
        assert!(Tab::Agents.companion_child());
        assert!(Tab::CompanionAi.companion_child());
        let secs = sections();
        for query in ["에이전트", "Claude Code", "Codex", "Antigravity", "절전", "말풍선"] {
            assert!(search(&secs, query).iter().any(|f| secs[f.section].tab == Tab::Agents), "{query}");
        }
    }

    #[test]
    fn search_finds_rows_by_their_label() {
        let found = |lang: &str, q: &str| -> Vec<(&'static str, Option<&'static str>)> {
            let _l = crate::i18n::test_lock(lang);
            let secs = sections();
            search(&secs, q).into_iter().map(|f| (secs[f.section].title, f.row.map(|r| secs[f.section].rows[r].key))).collect()
        };
        let rows = |v: &[(&str, Option<&'static str>)]| v.iter().filter_map(|f| f.1).collect::<Vec<_>>();
        // Case does not matter; the best match comes first.
        let r = found("en", "AUTOMATICALLY");
        assert_eq!(rows(&r).first(), Some(&"dock.autohide"));
        let r = found("en", "icon size");
        assert_eq!(r.first(), Some(&("settings.section.dock", Some("dock.icon_size"))));
        // Pages by their title too, and a tab by its name (its first page).
        assert!(found("en", "pinned").contains(&("settings.section.pins", None)));
        assert!(found("en", "shortcuts").contains(&("settings.section.shortcuts", None)));
        assert!(found("ko", "화면 모서리").contains(&("settings.tab.windows", Some(CORNERS_CARD))), "by its card's name");
        assert!(found("ko", "창 관리").contains(&("settings.tab.windows", None)), "by its sidebar name");
        // Rows under a fold are found (the fold opens on the jump).
        assert!(rows(&found("en", "keep windows from covering")).contains(&"dock.reserve_work_area"));
        assert!(rows(&found("en", "rounded square")).contains(&"dock.squircle"));
        let r = found("en", "Activity");
        assert!(rows(&r).contains(&"topbar.island"));
        // What was General's Search group: the Search & apps page now.
        assert!(found("en", "search with").contains(&("settings.tab.search", Some("search.with"))));
        assert!(found("en", "").is_empty());
        assert!(found("en", "zzzz").is_empty());
        // Accents: typed without them in French.
        assert!(rows(&found("fr", "demarrage")).contains(&"start_with_windows"));
        assert!(rows(&found("fr", "TACHES")).contains(&"hide_taskbar"));
        // Korean: words, initials, and a syllable still being typed.
        assert!(rows(&found("ko", "작업 표시줄")).contains(&"hide_taskbar"));
        assert!(rows(&found("ko", "ㅈㅇ ㅍㅅㅈ")).contains(&"hide_taskbar"));
        assert!(rows(&found("ko", "ㅇㅇㅋ")).contains(&"dock.icon_size"));
        assert!(rows(&found("ko", "아이콘 크")).contains(&"dock.icon_size"));
        assert!(rows(&found("ko", "숨기")).contains(&"dock.autohide"));
        assert!(found("ko", "독").contains(&("settings.section.dock", None)));
    }

    #[test]
    fn search_reads_descriptions_behind_labels() {
        let _l = crate::i18n::test_lock("en");
        let secs = sections();
        let keys = |q: &str| search(&secs, q).into_iter().filter_map(|f| f.row.map(|r| secs[f.section].rows[r].key)).collect::<Vec<_>>();
        // "Maximized" is only in the reserve-work-area row's description.
        assert!(keys("maximized").contains(&"dock.reserve_work_area"));
        // A label is found too.
        assert!(keys("magnification").contains(&"dock.max_scale"));
        // One letter is not looked for in descriptions (too many).
        assert!(!keys("x").contains(&"dock.reserve_work_area"));
    }

    #[test]
    fn the_search_narrows_a_page_to_its_matches() {
        let _l = crate::i18n::test_lock("en");
        let secs = sections();
        let bar = page(&secs, "settings.section.topbar");
        let at = |k: &str| bar.rows.iter().position(|r| r.key == k).unwrap();
        // Nothing typed, or the page's own name: the page whole.
        assert_eq!(page_filter(bar, ""), None);
        assert_eq!(page_filter(bar, "top bar"), None);
        // Nothing on the page matches: shown whole.
        assert_eq!(page_filter(bar, "zzzz"), None);
        // "Height" is inside Advanced: the fold shows, the rest of the page
        // does not.
        let f = page_filter(bar, "height").expect("a match");
        assert!(f[at("topbar.height")]);
        assert!(f[at("fold.topbar.advanced")], "its fold opens");
        assert!(!f[at("topbar.enabled")] && !f[at("topbar.island")]);
        // A heading that matches keeps its whole group.
        let general = page(&secs, "settings.section.general");
        let f = page_filter(general, "widgets").expect("the heading");
        let k = |key: &str| general.rows.iter().position(|r| r.key == key).unwrap();
        assert!(f[k("widgets.notes")] && f[k("widgets.hide_from_capture")] && !f[k("start_with_windows")]);
        // A fold none of whose rows match is left out.
        let dock = page(&secs, "settings.section.dock");
        let f = page_filter(dock, "square").expect("the squircle row");
        let fold = dock.rows.iter().position(|r| r.key == "fold.dock.advanced").unwrap();
        assert!(!f[fold]);
    }

    #[test]
    fn pin_list_reorder_math() {
        // Rows 56 apart from y = 100: the middle of the dragged row decides.
        assert_eq!(drop_slot(100.0, 100.0, 56.0, 4), 0);
        assert_eq!(drop_slot(155.9, 100.0, 56.0, 4), 0);
        assert_eq!(drop_slot(156.0, 100.0, 56.0, 4), 1);
        assert_eq!(drop_slot(40.0, 100.0, 56.0, 4), 0, "above the list: first");
        assert_eq!(drop_slot(900.0, 100.0, 56.0, 4), 3, "below it: last");
        assert_eq!(drop_slot(300.0, 100.0, 56.0, 0), 0);
        let mut v = vec!["A", "B", "C", "D"];
        assert!(move_item(&mut v, 0, 2));
        assert_eq!(v, ["B", "C", "A", "D"]);
        assert!(move_item(&mut v, 3, 0));
        assert_eq!(v, ["D", "B", "C", "A"]);
        assert!(!move_item(&mut v, 1, 1));
        assert!(!move_item(&mut v, 4, 0));
        assert!(!move_item(&mut v, 0, 4));
        assert_eq!(v, ["D", "B", "C", "A"]);
    }

    #[test]
    fn pins_added_in_the_window_merge_in_place() {
        // Window: added X after B and Y first. Dock meanwhile pinned C.
        let base = vec![pin("A"), pin("B"), pin("C")];
        let desired = vec![pin("Y"), pin("A"), pin("B"), pin("X")];
        let out = merge_pins(&base, &desired, &[], &["X".to_string(), "Y".to_string()]);
        assert_eq!(out.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(), ["Y", "A", "B", "X", "C"]);
        // Not marked as added: a pin the dock dropped meanwhile stays gone.
        let out = merge_pins(&[pin("A")], &[pin("A"), pin("Z")], &[], &[]);
        assert_eq!(out.len(), 1);
        // Already there: not twice.
        let out = merge_pins(&[pin("A"), pin("X")], &[pin("X"), pin("A")], &[], &["X".to_string()]);
        assert_eq!(out.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(), ["X", "A"]);
    }

    #[test]
    fn options_can_be_shown_as_they_are() {
        // Other tests switch the language: held for the comparison.
        let _l = crate::i18n::test_lock("en");
        assert_eq!(opt_text("raw:60"), "60");
        assert_eq!(opt_text("settings.size.small"), crate::i18n::t("settings.size.small"));
    }

    #[test]
    fn snapping() {
        assert_eq!(snap(1.26, 1.0, 2.5, 0.1), 1.3);
        assert_eq!(snap(99.0, 32.0, 96.0, 2.0), 96.0);
        assert_eq!(snap(33.0, 32.0, 96.0, 2.0), 34.0);
        assert_eq!(fmt_value(1.3, 0.1, "배"), "1.3배");
        assert_eq!(fmt_value(52.0, 2.0, ""), "52");
    }

    /// The tabs in the sidebar's order, each with its pages (titles).
    #[test]
    fn tabs_and_pages_in_order() {
        let secs = sections();
        // One page per sidebar entry, in the sidebar's order.
        let got: Vec<(Tab, Vec<&str>)> = Tab::ALL.iter().map(|t| (*t, pages_of(&secs, *t).into_iter().map(|i| secs[i].title).collect())).collect();
        let want: Vec<(Tab, Vec<&str>)> = vec![
            (Tab::General, vec!["settings.section.general"]),
            (Tab::Appearance, vec!["settings.section.appearance"]),
            (Tab::Dock, vec!["settings.section.dock", "settings.section.pins"]),
            (Tab::TopBar, vec!["settings.section.topbar", "settings.group.clock"]),
            (Tab::Launchpad, vec!["settings.tab.search"]),
            (Tab::Windows, vec!["settings.tab.windows"]),
            (Tab::Companion, vec!["companion.title"]),
            (Tab::CompanionAi, vec!["chat.ai.title"]),
            (Tab::Agents, vec!["settings.tab.agents"]),
            (Tab::Keyboard, vec!["settings.section.shortcuts", "settings.section.input"]),
            (Tab::About, vec!["settings.section.about"]),
        ];
        assert_eq!(got, want);
        // Ten entries, thirteen pages: a tab's pages follow each other.
        assert_eq!((Tab::ALL.len(), secs.len()), (11, 14));
        let mut order: Vec<Tab> = secs.iter().map(|s| s.tab).collect();
        order.dedup();
        assert_eq!(order, Tab::ALL.to_vec());
        // Three tabs with sub-pages, each with a page of its own name first.
        let multi: Vec<Tab> = Tab::ALL.iter().copied().filter(|t| pages_of(&secs, *t).len() > 1).collect();
        assert_eq!(multi, [Tab::Dock, Tab::TopBar, Tab::Keyboard]);
        // The clusters run in order, each in one piece.
        let clusters: Vec<u8> = Tab::ALL.iter().map(|t| t.cluster()).collect();
        assert!(clusters.windows(2).all(|w| w[0] <= w[1]), "{clusters:?}");
        assert_eq!(clusters.iter().collect::<std::collections::BTreeSet<_>>().len(), 3);
        // Every entry has a glyph of its own.
        let glyphs: std::collections::HashSet<_> = Tab::ALL.iter().map(|t| t.glyph()).collect();
        assert_eq!(glyphs.len(), Tab::ALL.len()-1);
    }

    /// Every row of every page (shortcuts and effects among them).
    fn all_rows() -> Vec<Row> {
        sections().into_iter().flat_map(|s| s.rows).collect()
    }

    #[test]
    fn every_description_is_behind_an_i_and_no_page_shows_it() {
        // Labels only: every row (and group label) with a description
        // carries an "i" for it; folds and rows without one carry none.
        let rows = all_rows();
        for r in &rows {
            let want = !r.hint.is_empty() && !matches!(r.ctl, Ctl::Fold { .. }) && !subtitled(r);
            assert_eq!(info_hint(r), want, "{}", r.key);
            assert!(!(info_hint(r) && subtitled(r)), "{}: never both", r.key);
        }
        let find = |k: &str| rows.iter().find(|r| r.key == k).unwrap_or_else(|| panic!("{k}: no row"));
        for k in ["hide_taskbar", "action.launchpad_reset", "theme.accent", "shortcuts.search", LOW_SPEC, "dock.position", "start_with_windows", "action.quit"] {
            assert!(info_hint(find(k)), "{k}");
        }
        assert!(info_hint(find(THIS_DOCK)));
        assert!(!info_hint(find("dock.indicator")));
        // What a label alone does not say: under it, no "i".
        for k in SUBTITLED {
            let r = find(k);
            assert!(subtitled(r) && !info_hint(r), "{k}: a grey line under its label");
        }
        // The first-run flow's rows too.
        for r in welcome_rows(1) {
            assert_eq!(info_hint(&r), !r.hint.is_empty(), "{}", r.key);
        }
    }

    #[test]
    fn a_row_s_name_is_a_name_not_a_sentence() {
        // A label is a short name ("Quit NopBar"), its button a verb
        // ("Quit"), what it does in the "i": no label ends a sentence.
        for lang in ["en", "ko"] {
            let _l = crate::i18n::test_lock(lang);
            for r in all_rows().iter().filter(|r| !matches!(r.ctl, Ctl::Header | Ctl::Fold { .. })) {
                let l = crate::i18n::t(r.label).trim_end();
                assert!(!l.ends_with('.') && !l.ends_with('。') && !l.ends_with("요") && !l.ends_with("다"), "{lang} {}: {l:?}", r.key);
            }
        }
    }

    #[test]
    fn every_description_still_reads_for_a_screen_reader() {
        // The row's help (access.rs: row_help) is its description, shown or
        // not: each one has text in English and Korean.
        for lang in ["en", "ko"] {
            let _l = crate::i18n::test_lock(lang);
            for r in all_rows().iter().filter(|r| !r.hint.is_empty()) {
                assert!(!crate::i18n::t(r.hint).trim().is_empty(), "{lang}: {}", r.key);
            }
            // Optional page help remains available to screen readers; label-only
            // pages intentionally have no introductory sentence.
            for s in sections().into_iter().filter(|s| !s.intro.is_empty()) {
                assert!(!crate::i18n::t(s.intro).trim().is_empty(), "{lang}: {}", s.title);
            }
        }
    }

    #[test]
    fn the_sidebar_lays_out_in_clusters_and_scrolls_when_short() {
        let (tops, h) = sidebar_tops(&Tab::ALL, 32.0, 12.0);
        assert_eq!(tops.len(), Tab::ALL.len());
        assert_eq!(tops[0], 0.0);
        // Within a cluster one pitch apart, between two a gap more: three
        // clusters (NopBar and its look | what it puts on screen | the
        // keyboard, updates, about).
        assert_eq!(tops[1] - tops[0], 32.0, "General, Appearance");
        assert_eq!(tops[2] - tops[1], 44.0, "Appearance, then Dock (next cluster)");
        assert_eq!(tops[6] - tops[5], 32.0, "Windows, Agents: one cluster");
        assert_eq!(tops[9] - tops[8], 44.0, "Companion, then Keyboard (last cluster)");
        assert_eq!(tops[3] - tops[2], 32.0, "Dock, Top bar: one cluster");
        assert_eq!(h, 11.0 * 32.0 + 2.0 * 12.0);
        // The window's own sizes: ten entries fit even the smallest one
        // (the list still scrolls when the window is shorter).
        use super::super::{reveal_span, side_scroll_max, CLUSTER_GAP, H0, MIN_H, TAB_PITCH};
        let (_, real) = sidebar_tops(&Tab::ALL, TAB_PITCH, CLUSTER_GAP);
        assert_eq!(side_scroll_max(real, H0), 0.0, "it all fits when first opened");
        assert_eq!(side_scroll_max(real, MIN_H), 0.0, "fits the smallest window");
        assert!(side_scroll_max(real, real / 2.0) > 0.0);
        assert_eq!(side_scroll_max(h, 2000.0), 0.0);
        // An entry out of sight is scrolled to, just far enough.
        assert_eq!(reveal_span(0.0, 40.0, 30.0, 300.0), 0.0, "in sight");
        assert_eq!(reveal_span(0.0, 400.0, 30.0, 300.0), 138.0);
        assert_eq!(reveal_span(200.0, 100.0, 30.0, 300.0), 96.0);
        assert!(sidebar_tops(&[], 30.0, 12.0).0.is_empty());
    }

    /// Config fields no row shows, and why (anything else must have
    /// exactly one row: a new setting cannot be forgotten, or listed twice).
    const NOT_SHOWN: [(&str, &str); 35] = [
        ("native_defaults_version", "one-time native defaults migration"),
        (
            "chat.provider",
            "single active provider selected by the AI card",
        ),
        ("agents.claude", "controlled together with chat.claude_mcp"),
        ("agents.codex", "controlled together with chat.codex_mcp"),
        ("chat.claude_model", "provider model picker"),
        ("chat.antigravity_model", "provider model picker"),
        ("chat.claude_connected", "remembered CLI connection"),
        ("chat.antigravity_connected", "remembered CLI connection"),
        (
            "chat.model",
            "account catalog picker in the AI connection section",
        ),
        (
            "companion.size_scale", "continuous value controlled by the companion.size slider",
        ),
        ("companion.snooze_until", "wall clock return deadline, set by the companion menu",
        ),
        ("companion.activity_budget", "numeric daily reaction caps"),
        ("companion.routine", "numeric usage history and daily wrap gate"),
        ("companion.last_greet", "local date of the last morning greeting, set by the companion",
        ),
        ("companion.home", "monitor, optional edge and position along it, set by dragging the companion",
        ),
        ("topbar.height_defaults", "the height format of the file (config::migrate_removed)",
        ),
        ("settings_window", "the settings window's last size"),
        ("appearance.font_defaults", "the font format of the file (config::migrate_font)"),
        ("version", "file format"),
        ("welcomed", "first run, internal"),
        ("welcome_done", "first run, internal"),
        ("animation.launch", "older builds' launch animation"),
        ("updates.manifest_url", "testing only"),
        ("topbar.tray_hidden", "set by dragging tray icons"),
        ("topbar.tray_visible", "set by dragging tray icons"),
        ("topbar.tray_order", "set by dragging tray icons"),
        ("dock.main_display", "set by the dock when the main display changes"),
        ("effects.render", "config file or NOPBAR_RENDER only (for measuring)"),
        ("topbar.reserve_work_area", "hidden: older files' topbar.autohide"),
        ("launchpad.layout", "set by arranging Launchpad; its Reset button empties it"),
        ("launchpad.schema", "set by Launchpad's migration; its Reset button clears it"),
        ("launchpad.seen", "set by Launchpad (which apps are new)"),
        ("launchpad.library", "set by Launchpad's Move to category"),
        ("launchpad.hidden", "set by Launchpad's Hide / Unhide"),
        ("launchpad.cleanup_dismissed", "set by Launchpad's banner; the Clean up suggestions button clears it"),
    ];

    /// Every leaf of a config as "a.b" paths (maps and lists are one leaf).
    fn leaves(v: &serde_json::Value, prefix: &str, out: &mut Vec<String>) {
        match v {
            serde_json::Value::Object(m) if !m.is_empty() && !matches!(prefix, "shortcuts" | "dock.icon_overrides" | "launchpad.library" | "companion.home" | "companion.routine" | "companion.activity_budget") => {
                for (k, x) in m {
                    let p = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
                    leaves(x, &p, out);
                }
            }
            _ => out.push(prefix.to_string()),
        }
    }

    /// The config fields a page's setting key stands for.
    fn fields(key: &str) -> Vec<String> {
        match key {
            "theme" => vec!["theme.ref".into()],
            // A part of dock.pins (the Pinned apps page's list).
            "launchpad.pin" => Vec::new(),
            // One control for the finish and the blur.
            "theme.finish" => vec!["theme.finish".into(), "effects.blur".into()],
            // Scrolling on the dock and on the bar: one switch.
            "dock.wheel_desktops" => vec!["dock.wheel_desktops".into(), "topbar.wheel_desktops".into()],
            k if k.starts_with("shortcuts.") => vec!["shortcuts".into()],
            k => vec![k.to_string()],
        }
    }


    #[test]
    fn every_config_field_has_exactly_one_row() {
        // Fields written only when set are set, so they are listed too.
        let mut c = Config::default();
        c.topbar.reserve_work_area = false;
        c.updates.manifest_url = Some("https://example.invalid/update.json".into());
        c.launchpad.layout = vec![vec![crate::config::LpItem::App("x".into())]];
        c.launchpad.schema = 1;
        c.launchpad.show_library = false;
        c.launchpad.seen = vec!["x".into()];
        c.launchpad.library.insert("x".into(), "games".into());
        c.launchpad.hidden = vec!["y".into()];
        c.launchpad.cleanup_dismissed = true;
        c.settings_window = Some([800, 600]);
        c.companion.home = Some(crate::companion::grab::Home { monitor: 1, ratio: 0.5, edge: None });
        let json = serde_json::to_value(c).unwrap();
        let mut all = Vec::new();
        leaves(&json, "", &mut all);
        // Fields whose row went (the declutter) are hidden too.
        let hidden: Vec<&str> = NOT_SHOWN.iter().map(|(k, _)| *k).chain(crate::config::DEFAULTED.iter().map(|(k, _)| *k)).collect();
        for h in &hidden {
            assert!(all.iter().any(|f| f == h), "{h} is not a config field any more");
        }
        // Which page owns each field (a page counts once per field).
        let mut owner: std::collections::HashMap<String, Vec<&str>> = Default::default();
        let secs = sections();
        for s in &secs {
            let mut mine: Vec<String> = page_keys(s).iter().flat_map(|k| fields(k)).collect();
            mine.sort();
            mine.dedup();
            for f in mine {
                assert!(all.contains(&f), "{}: {f} is not a config field", s.title);
                owner.entry(f).or_default().push(s.title);
            }
        }
        for f in &all {
            if hidden.contains(&f.as_str()) {
                assert!(!owner.contains_key(f), "{f} is listed as not shown but has a row");
                continue;
            }
            let pages = owner.get(f).map(|v| v.as_slice()).unwrap_or(&[]);
            assert_eq!(pages.len(), 1, "{f}: on {pages:?} (no row: add one or list it in NOT_SHOWN)");
        }
        // And within the pages, one row per key.
        let mut seen = std::collections::HashSet::new();
        for s in &secs {
            for k in page_keys(s) {
                assert!(seen.insert(k), "{k} twice");
            }
        }
    }

    #[test]
    fn search_finds_every_row() {
        for lang in ["en", "ko"] {
            let _l = crate::i18n::test_lock(lang);
            let secs = sections();
            for (si, s) in secs.iter().enumerate() {
                for (ri, r) in s.rows.iter().enumerate() {
                    if matches!(r.ctl, Ctl::Fold { .. }) {
                        continue;
                    }
                    let hits = search(&secs, crate::i18n::t(r.label));
                    assert!(hits.contains(&Found { section: si, row: Some(ri) }), "{lang}: {} not found by its label", r.key);
                }
            }
            // Every page too.
            for (si, s) in secs.iter().enumerate() {
                assert!(search(&secs, crate::i18n::t(s.title)).contains(&Found { section: si, row: None }), "{lang}: page {}", s.title);
            }
        }
    }

    #[test]
    fn folds_hold_settings_only() {
        for s in sections() {
            for (fi, r) in s.rows.iter().enumerate() {
                let Ctl::Fold { n } = r.ctl else { continue };
                assert!(n > 0 && fi + n < s.rows.len(), "{}: {} runs past the page", s.title, r.key);
                for x in &s.rows[fi + 1..=fi + n] {
                    assert!(!matches!(x.ctl, Ctl::Header | Ctl::Fold { .. }), "{}: {} inside {}", s.title, x.key, r.key);
                    assert_eq!(fold_of(&s.rows, s.rows.iter().position(|y| y.key == x.key).unwrap()), Some(fi));
                }
                assert!(r.key.starts_with("fold."), "{}", r.key);
            }
        }
        // What sits under "Advanced" on the Dock page.
        let secs = sections();
        let dock = page(&secs, "settings.section.dock");
        let fi = dock.rows.iter().position(|r| r.key == "fold.dock.advanced").unwrap();
        let inside: Vec<&str> = dock.rows[fi + 1..].iter().map(|r| r.key).collect();
        assert_eq!(inside, ["dock.reserve_work_area", "dock.display_apps_only"]);
        assert_eq!(fold_of(&dock.rows, 0), None);
        // The icons' shape is the dock's face: with the dock's items.
        let sq = dock.rows.iter().position(|r| r.key == "dock.squircle").unwrap();
        assert!(sq < fi && sq > dock.rows.iter().position(|r| r.key == ALL_DOCKS).unwrap());
        // The bar's items are no folds of options any more (their parts
        // keep their defaults).
        let bar = page(&secs, "settings.section.topbar");
        assert!(bar.rows.iter().filter(|r| matches!(r.ctl, Ctl::Fold { .. })).all(|r| r.label == "settings.fold.advanced"));
    }

    #[test]
    fn the_focus_timer_is_a_card_of_the_clock() {
        let secs = sections();
        let clock = page(&secs, "settings.group.clock");
        assert_eq!(clock.tab, Tab::TopBar, "a sub-page of the Top bar");
        let keys: Vec<&str> = clock.rows.iter().skip_while(|r| r.key != "group.clock.timer").skip(1).take_while(|r| !matches!(r.ctl, Ctl::Header)).map(|r| r.key).collect();
        // Its switch in the clock menu leads the card.
        assert_eq!(keys, ["focus_timer.enabled", "focus_timer.custom_focus", "focus_timer.custom_break", "focus_timer.dnd", "focus_timer.chime"], "one switch to be told it ended");
        // The calendars are the page's last heading (their list follows).
        assert_eq!(clock.rows.last().map(|r| r.key), Some("group.clock.calendar"));
        let bar = page(&secs, "settings.section.topbar");
        assert!(!bar.rows.iter().any(|r| r.key.starts_with("focus_timer.")));
        // "--page topbar" (the clock menu) finds the timer's first row there.
        let (si, _) = find_row(&secs, super::super::TIMER_ROW).unwrap();
        assert_eq!(secs[si].title, "settings.group.clock");
    }

    #[test]
    fn magnification_in_four_steps() {
        assert_eq!(MAGNIFY_STEPS, [1.0, 1.4, 1.6, 2.0]);
        assert_eq!(MAGNIFY_STEPS[magnify_index(Config::default().dock.max_scale)], 1.0, "the default is a step: Off");
        for (v, i) in [(1.0, 0), (1.04, 0), (1.1, 1), (1.3, 1), (1.4, 1), (1.5, 1), (1.55, 2), (1.6, 2), (1.8, 2), (1.85, 3), (2.0, 3), (2.5, 3)] {
            assert_eq!(magnify_index(v), i, "{v}");
        }
        let secs = sections();
        let (si, ri) = find_row(&secs, "dock.max_scale").unwrap();
        let Ctl::Choice { opts, get, set } = secs[si].rows[ri].ctl else { panic!("a choice") };
        assert_eq!(opts.len(), 4);
        let mut c = Config::default();
        set(&mut c, 3);
        assert_eq!((c.dock.max_scale, get(&c)), (2.0, 3));
        // Sent by its key, the dock gets the step.
        let mut dock = Config::default();
        apply_keys(&c, &mut dock, &["dock.max_scale".to_string()]);
        assert_eq!(dock.dock.max_scale, 2.0);
    }

    #[test]
    fn the_theme_rows_travel_by_value() {
        let mut c = Config::default();
        assert_eq!((c.theme.mode_index(), c.theme.accent_index()), (0, 0), "the theme's own: Graphite follows Windows");
        c.theme.mode = "dark".into();
        c.theme.accent = "#30D158".into();
        assert_eq!((c.theme.mode_index(), c.theme.accent_index()), (2, 7));
        let mut dock = Config::default();
        apply_keys(&c, &mut dock, &["theme.mode".to_string(), "theme.accent".to_string()]);
        assert_eq!((dock.theme.mode.as_str(), dock.theme.accent.as_str()), ("dark", "#30D158"));
        // A color of one's own travels as it is (its index is "Custom…").
        let mut own = c.clone();
        own.theme.accent = "#12AB3C".into();
        apply_keys(&own, &mut dock, &["theme.accent".to_string()]);
        assert_eq!(dock.theme.accent, "#12AB3C");
        assert_eq!(dock.theme.accent_index(), crate::config::ACCENT_CUSTOM);
        dock.theme.accent = "#30D158".into();
        assert_eq!(dock.theme.source, "builtin:graphite", "the file is its own key");
        // Over the theme: a fixed mode and accent.
        let t = dock.theme.load(std::path::Path::new("."));
        assert_eq!((t.mode.as_str(), t.accent.as_str()), ("dark", "#30D158"));
        // The file choice alone leaves them.
        let mut other = c.clone();
        other.theme.source = "file:themes\\x.json".into();
        other.theme.mode = "light".into();
        apply_keys(&other, &mut dock, &["theme".to_string()]);
        assert_eq!((dock.theme.source.as_str(), dock.theme.mode.as_str()), ("file:themes\\x.json", "dark"));
        // The window's size travels as it is.
        c.settings_window = Some([900, 640]);
        apply_keys(&c, &mut dock, &["settings_window".to_string()]);
        assert_eq!(dock.settings_window, Some([900, 640]));
    }

    /// New configs: the core on, the noisy extras off.
    #[test]
    fn defaults_table() {
        let c = Config::default();
        let rows = setting_rows();
        let on = |k: &str| match rows.iter().find(|r| r.key == k).map(|r| &r.ctl) {
            Some(Ctl::Toggle { get, .. }) => get(&c),
            Some(Ctl::Choice { get, .. }) | Some(Ctl::Menu { get, .. }) => get(&c) == 0,
            _ => panic!("{k}"),
        };
        let want = [
            ("topbar.island", true),
            ("topbar.same_on_all_displays", false),
            ("topbar.island_screenshot", false),
            ("focus_timer.enabled", true),
            ("topbar.clock_seconds", false),
            ("topbar.show_wifi", false),
            ("topbar.show_volume", false),
            ("topbar.show_battery", true),
            ("dock.indicator", true),
            ("dock.wheel_desktops", false),
            ("topbar.show_desktops", true),
            ("input.indicator", true),
        ];
        for (k, v) in want {
            assert_eq!(on(k), v, "{k}");
        }
        assert_eq!(c.dock.display_mode(), crate::config::DisplayMode::Every, "dock on every display");
        assert!(!c.dock.display_apps_only && c.focus_timer.dnd);
        assert_eq!(c.input.indicator, "auto", "only with an IME or more than one layout");
        assert_eq!(c.topbar.show_desktops, "auto", "only with more than one desktop");
        assert!(!c.hot_corners.any(), "no corner does anything until chosen");
        assert!(!c.input.remember);
        assert!(c.widgets.notes && c.widgets.pieces && !c.widgets.hide_from_capture);
        assert!(c.topbar.enabled && c.start_with_windows && c.hide_taskbar && c.tray_icon);
    }

    const MON: &str = "DEL40F2-ABC";

    fn on_display() -> Editing {
        Editing { key: MON.into(), name: "DELL U2720Q".into() }
    }

    fn every() -> Config {
        let mut c = Config::default();
        c.dock.set_display_mode(2);
        c
    }

    fn row_of(key: &str) -> Row {
        sections().into_iter().flat_map(|s| s.rows).find(|r| r.key == key).unwrap_or_else(|| panic!("no row {key}"))
    }

    #[test]
    fn display_rows_set_the_picked_display() {
        let mut c = every();
        c.dock.icon_size = 52.0;
        let Ctl::Toggle { get: same, set: set_same } = row_of(SAME_AS_MAIN).ctl else { panic!() };
        let Ctl::Slider { get: size, set: set_size, .. } = row_of("dock.icon_size").ctl else { panic!() };
        let Ctl::Choice { get: pos, set: set_pos, .. } = row_of("dock.position").ctl else { panic!() };
        // The main dock: the rows set its fields; it is always "the same".
        edit_display(None);
        assert!(same(&c));
        set_size(&mut c, 60.0);
        assert_eq!(c.dock.icon_size, 60.0);
        // Another display, same as the main dock: its rows show the main
        // dock's values and set them (the window locks them meanwhile).
        edit_display(Some(on_display()));
        assert!(same(&c));
        assert_eq!(size(&c), 60.0);
        // Its own settings: a copy of the main dock's to start from.
        set_same(&mut c, false);
        assert!(!same(&c) && editing_own(&c));
        let own = c.dock.per_display.get(MON).unwrap();
        assert_eq!((own.icon_size, own.position, own.name.as_str()), (Some(60.0), Some(Position::Bottom), "DELL U2720Q"));
        assert_eq!(own.pins.as_deref(), Some(c.dock.pins.as_slice()));
        // Now its rows set only it.
        set_size(&mut c, 80.0);
        set_pos(&mut c, 1);
        assert_eq!(size(&c), 80.0);
        assert_eq!(pos(&c), 1);
        assert_eq!(c.dock.icon_size, 60.0, "the main dock keeps its own");
        assert_eq!(c.dock.position, Position::Bottom);
        // The main dock's rows again.
        edit_display(None);
        assert_eq!(size(&c), 60.0);
        // Same again: its own settings go.
        edit_display(Some(on_display()));
        set_same(&mut c, true);
        assert!(c.dock.per_display.is_empty());
        assert_eq!(size(&c), 60.0);
        edit_display(None);
    }

    #[test]
    fn display_docks_travel_by_their_map_key() {
        // The window edited display MON's dock: only "dock.per_display" is
        // sent; the row keys copy the main dock's fields whatever the
        // window had picked.
        let mut from = every();
        from.dock.per_display.insert(MON.into(), DisplayDock { position: Some(Position::Left), icon_size: Some(70.0), ..Default::default() });
        from.dock.icon_size = 44.0;
        let mut to = every();
        edit_display(Some(on_display()));
        apply_keys(&from, &mut to, &["dock.per_display".to_string()]);
        assert_eq!(to.dock.per_display, from.dock.per_display);
        assert_eq!(to.dock.icon_size, 44.0, "the main dock's size was not sent");
        apply_keys(&from, &mut to, &["dock.icon_size".to_string()]);
        assert_eq!(to.dock.icon_size, 44.0, "the row key is the main dock's field");
        assert_eq!(editing(), Some(on_display()), "the pick survives apply_keys");
        edit_display(None);
    }

    #[test]
    fn display_pins_merge_with_changes_made_meanwhile() {
        // The window removed B from display MON's own list; the dock
        // meanwhile pinned C there.
        let mut from = every();
        from.dock.per_display.insert(MON.into(), DisplayDock { pins: Some(vec![pin("A")]), ..Default::default() });
        let mut to = every();
        to.dock.per_display.insert(MON.into(), DisplayDock { pins: Some(vec![pin("A"), pin("B"), pin("C")]), ..Default::default() });
        apply_keys(&from, &mut to, &["dock.per_display".to_string(), format!("-pin@{MON}:B")]);
        let got: Vec<String> = to.dock.per_display[MON].pins.as_ref().unwrap().iter().map(|p| p.path.clone()).collect();
        assert_eq!(got, ["A", "C"]);
    }

    #[test]
    fn pins_page_reset_resets_the_list_shown() {
        let secs = sections();
        let pins_page = secs.iter().find(|s| s.kind == Kind::Pins).unwrap();
        let defaults = defaults(vec![pin("L"), pin("edge.lnk")]);
        let mut c = every();
        c.dock.pins = vec![pin("L"), pin("main.exe")];
        c.dock.per_display.insert(MON.into(), DisplayDock { pins: Some(vec![pin("mine.exe"), pin("main.exe")]), ..Default::default() });
        c.dock.icon_overrides.insert("mine.exe".into(), "icons\\m.png".into());
        c.dock.icon_overrides.insert("main.exe".into(), "icons\\x.png".into());
        // Display MON's own list is shown: that list is reset.
        edit_display(Some(on_display()));
        let keys = reset_page(&mut c, &defaults, pins_page);
        edit_display(None);
        let own: Vec<&str> = c.dock.per_display[MON].pins.as_ref().unwrap().iter().map(|p| p.path.as_str()).collect();
        assert_eq!(own, ["L", "edge.lnk"]);
        assert_eq!(paths(&c), ["L", "main.exe"], "the main dock's apps stay");
        assert!(!c.dock.icon_overrides.contains_key("mine.exe"), "its own app's icon goes");
        assert!(c.dock.icon_overrides.contains_key("main.exe"), "the main dock still has that app");
        for k in ["dock.per_display", "-pin@DEL40F2-ABC:mine.exe", "+pin@DEL40F2-ABC:L", "-icon:mine.exe"] {
            assert!(keys.iter().any(|x| x == k), "{k} in {keys:?}");
        }
        assert!(!keys.iter().any(|k| k == "dock.pins" || k.starts_with("-pin:")), "{keys:?}");
        // Sent to the dock (which pinned new.exe there meanwhile).
        let mut dock = every();
        dock.dock.per_display.insert(MON.into(), DisplayDock { pins: Some(vec![pin("mine.exe"), pin("main.exe"), pin("new.exe")]), ..Default::default() });
        apply_keys(&c, &mut dock, &keys);
        let got: Vec<&str> = dock.dock.per_display[MON].pins.as_ref().unwrap().iter().map(|p| p.path.as_str()).collect();
        assert_eq!(got, ["L", "edge.lnk", "new.exe"], "the reset list, then what the dock pinned meanwhile");
        // The main list: an icon another display still uses stays.
        let mut c = every();
        c.dock.pins = vec![pin("shared.exe")];
        c.dock.per_display.insert(MON.into(), DisplayDock { pins: Some(vec![pin("shared.exe")]), ..Default::default() });
        c.dock.icon_overrides.insert("shared.exe".into(), "icons\\s.png".into());
        reset_page(&mut c, &defaults, pins_page);
        assert!(c.dock.icon_overrides.contains_key("shared.exe"));
    }

    #[test]
    fn dock_page_reset_keeps_each_displays_apps() {
        let secs = sections();
        let dock = secs.iter().find(|s| s.kind == Kind::Dock).unwrap();
        assert!(page_keys(dock).contains(&"dock.per_display"));
        assert!(!page_keys(dock).contains(&SAME_AS_MAIN), "a switch, not a field");
        let mut c = every();
        c.dock.per_display.insert(MON.into(), DisplayDock { name: "DELL".into(), position: Some(Position::Right), autohide: Some(true), pins: Some(vec![pin("x.exe")]), ..Default::default() });
        reset_page(&mut c, &defaults(Vec::new()), dock);
        let d = &c.dock.per_display[MON];
        assert_eq!((d.position, d.autohide), (None, None), "settings follow the main dock again");
        assert_eq!(d.pins.as_ref().map(|p| p.len()), Some(1), "its apps stay");
    }

    #[test]
    fn the_bar_can_move_to_the_bottom() {
        let secs = sections();
        let bar = page(&secs, "settings.section.topbar");
        let at = |k: &str| bar.rows.iter().position(|r| r.key == k).unwrap();
        assert_eq!(at("topbar.position"), at("topbar.enabled") + 1, "right under the bar's switch");
        assert_eq!(fold_of(&bar.rows, at("topbar.position")), None, "not under Advanced");
        let Ctl::Choice { opts, get, set } = row_of("topbar.position").ctl else { panic!() };
        assert_eq!(opts, ["settings.topbar.position.top", "settings.topbar.position.bottom"]);
        let mut c = Config::default();
        assert_eq!(get(&c), 0, "on top by default");
        set(&mut c, 1);
        assert_eq!(c.topbar.position, "bottom");
        assert!(c.topbar.at_bottom());
        // Sent by its key, the dock gets the same (applied live).
        let mut dock = Config::default();
        apply_keys(&c, &mut dock, &["topbar.position".to_string()]);
        assert!(dock.topbar.at_bottom());
        set(&mut c, 9);
        assert_eq!(c.topbar.position, "bottom", "out of range: the last option");
        // Found by the words people use for it.
        for (lang, words) in [("en", ["bottom", "taskbar"]), ("ko", ["아래", "작업 표시줄"])] {
            let _l = crate::i18n::test_lock(lang);
            let secs = sections();
            for q in words {
                let keys: Vec<&str> = search(&secs, q).into_iter().filter_map(|f| f.row.map(|r| secs[f.section].rows[r].key)).collect();
                assert!(keys.contains(&"topbar.position"), "{lang} {q}: {keys:?}");
            }
        }
        // With the bar off, the row says why it does nothing.
        let mut off = Config::default();
        off.topbar.enabled = false;
        assert_eq!(feature_note(&off, "topbar.position"), Some("features.note.bar_off"));
    }

    #[test]
    fn windows_feel_indicator_choices_round_trip() {
        let Ctl::Choice { opts, get, set } = row_of("dock.indicator").ctl else { panic!() };
        assert_eq!(opts.len(), 3);
        let mut c = Config::default();
        for i in 0..3 {
            set(&mut c, i);
            let read = crate::config::from_json(&serde_json::to_string(&c).unwrap()).unwrap();
            assert_eq!(get(&read), i);
        }
    }

    #[test]
    fn the_bars_background_is_one_row_under_its_position() {
        let secs = sections();
        let bar = page(&secs, "settings.section.topbar");
        let at = |k: &str| bar.rows.iter().position(|r| r.key == k).unwrap();
        assert_eq!(at("topbar.style"), at("topbar.position") + 1);
        assert_eq!(fold_of(&bar.rows, at("topbar.style")), None, "not under Advanced");
        let Ctl::Menu { opts, get, set } = row_of("topbar.style").ctl else { panic!() };
        assert_eq!(opts.len(), TOPBAR_STYLES.len());
        let mut c = Config::default();
        c.theme.finish = "glass".into();
        assert_eq!(get(&c), 4, "default adaptive glass on Glass");
        c.theme.finish = "flat".into();
        assert_eq!(get(&c), 4, "default adaptive glass on Flat");
        for (i, name) in TOPBAR_STYLES.iter().enumerate() {
            set(&mut c, i);
            assert_eq!((c.topbar.style.as_str(), get(&c)), (*name, i));
        }
        set(&mut c, 9);
        assert_eq!(c.topbar.style, "glass", "out of range: the last option");
        let mut dock = Config::default();
        apply_keys(&c, &mut dock, &["topbar.style".to_string()]);
        assert_eq!(dock.topbar.style, "glass", "sent by its key");
        // Plain words in Korean.
        let _l = crate::i18n::test_lock("ko");
        let words: Vec<&str> = opts.iter().map(|k| crate::i18n::t(k)).collect();
        assert_eq!(words, ["투명", "서리", "재질 따라가기", "불투명", "자동 밝기 유리"]);
    }

    #[test]
    fn seconds_sit_with_the_clocks_other_option() {
        let secs = sections();
        // On the Top bar's Clock sub-page, under its Clock heading.
        let clock = page(&secs, "settings.group.clock");
        let at = |k: &str| clock.rows.iter().position(|r| r.key == k).unwrap();
        assert_eq!(at("topbar.clock_seconds"), at("group.clock.clock") + 1);
        assert_eq!(fold_of(&clock.rows, at("topbar.clock_seconds")), None, "not under Advanced");
        let Ctl::Toggle { get, set } = row_of("topbar.clock_seconds").ctl else { panic!() };
        let mut c = Config::default();
        assert!(!get(&c), "off by default");
        set(&mut c, true);
        assert!(c.topbar.clock_seconds);
        // Sent by its key, the dock gets the same (applied live).
        let mut dock = Config::default();
        apply_keys(&c, &mut dock, &["topbar.clock_seconds".to_string()]);
        assert!(dock.topbar.clock_seconds);
        // Found by the words people use for it.
        for (lang, words) in [("en", ["seconds", "clock"]), ("ko", ["초", "시계"])] {
            let _l = crate::i18n::test_lock(lang);
            let secs = sections();
            for q in words {
                let keys: Vec<&str> = search(&secs, q).into_iter().filter_map(|f| f.row.map(|r| secs[f.section].rows[r].key)).collect();
                assert!(keys.contains(&"topbar.clock_seconds"), "{lang} {q}: {keys:?}");
            }
        }
        let mut off = Config::default();
        off.topbar.enabled = false;
        assert_eq!(feature_note(&off, "topbar.clock_seconds"), Some("features.note.bar_off"));
    }

    #[test]
    fn search_finds_display_settings_by_other_words() {
        for (lang, words) in [("en", ["display", "monitor"]), ("ko", ["디스플레이", "모니터"])] {
            let _l = crate::i18n::test_lock(lang);
            let secs = sections();
            for q in words {
                let keys: Vec<&str> = search(&secs, q).into_iter().filter_map(|f| f.row.map(|r| secs[f.section].rows[r].key)).collect();
                assert!(keys.contains(&"dock.displays"), "{lang} {q}: {keys:?}");
                assert!(keys.contains(&"topbar.same_on_all_displays"), "{lang} {q}: {keys:?}");
            }
        }
    }

    #[test]
    fn bar_picture_links_to_top_bar_rows() {
        let secs = sections();
        let bar = page(&secs, "settings.section.topbar");
        for k in super::super::topbar_view::linked_keys() {
            assert!(bar.rows.iter().any(|r| r.key == k), "{k} not on the Top bar page");
        }
    }

    #[test]
    fn cards_say_why_they_do_not_show() {
        let mut c = Config::default();
        assert_eq!(feature_note(&c, "topbar.island"), None);
        c.topbar.island = false;
        assert_eq!(feature_note(&c, "topbar.island_screenshot"), Some("features.note.island_off"));
        c.topbar.enabled = false;
        for k in ["topbar.island", "topbar.show_desktops", "topbar.island_screenshot", "input.indicator"] {
            assert_eq!(feature_note(&c, k), Some("features.note.bar_off"), "{k}");
        }
        assert_eq!(feature_note(&c, "widgets.notes"), None, "not on the bar");
        for k in ["features.note.bar_off", "features.note.island_off"] {
            assert!(crate::i18n::get(k).is_some(), "{k}");
        }
    }

    #[test]
    fn a_dock_takes_apps_of_its_own_only_when_asked() {
        let mut c = every();
        c.dock.pins = vec![pin("a.exe")];
        edit_display(None);
        assert!(!customize_pins(&mut c), "the main dock has its own");
        edit_display(Some(on_display()));
        assert!(customize_pins(&mut c));
        assert!(!same_as_main(&c));
        assert_eq!(c.dock.per_display[MON].pins.as_deref(), Some(c.dock.pins.as_slice()));
        assert!(!customize_pins(&mut c), "once");
        // An entry of settings only gets the apps.
        c.dock.per_display.insert(MON.into(), DisplayDock { autohide: Some(true), ..Default::default() });
        assert!(customize_pins(&mut c));
        assert_eq!(c.dock.per_display[MON].autohide, Some(true));
        assert!(c.dock.per_display[MON].pins.is_some());
        edit_display(None);
    }

    #[test]
    fn launchpad_reset_sits_with_launchpad() {
        let secs = sections();
        let rows = &page(&secs, "settings.tab.search").rows;
        let at = |k: &str| rows.iter().position(|r| r.key == k).unwrap();
        assert!(at("action.launchpad_reset") > at("launchpad.show_library"), "under Launchpad's other settings");
        assert_eq!(at("action.launchpad_reset"), rows.len() - 1, "last: it is the one that undoes most");
        assert!(matches!(rows[at("action.launchpad_reset")].ctl, Ctl::Button { .. }));
        // Its commit empties the arrangement and nothing else.
        let mut dock = Config::default();
        dock.launchpad.layout = vec![vec![crate::config::LpItem::App("a".into())]];
        dock.hide_taskbar = false;
        apply_keys(&Config::default(), &mut dock, &["launchpad.layout".to_string()]);
        assert!(dock.launchpad.layout.is_empty());
        assert!(!dock.hide_taskbar, "the rest untouched");
    }

    #[test]
    fn font_and_text_size_rows() {
        let secs = sections();
        let look = page(&secs, "settings.section.appearance");
        let keys = page_keys(look);
        let at = |k: &str| keys.iter().position(|x| *x == k).unwrap();
        assert_eq!((at("appearance.font"), at("appearance.text_size")), (at("theme.accent") + 1, at("theme.accent") + 2));
        // The theme file is under Advanced, last.
        let fold = look.rows.iter().position(|r| r.key == "fold.look.advanced").expect("Advanced");
        let file = look.rows.iter().position(|r| r.key == "theme").unwrap();
        assert_eq!(fold_of(&look.rows, file), Some(fold));
        assert_eq!(look.rows.last().map(|r| r.key), Some("action.themes_folder"));
        for k in ["appearance.font", "appearance.text_size"] {
            assert!(crate::i18n::get(&format!("search.alias.{k}")).is_some(), "{k}");
        }
        // Pretendard (the default) first, Windows' font, then the
        // families; an unknown one reads as the default.
        assert_eq!(&font_opts()[..2], ["settings.appearance.font.default", "settings.appearance.font.system"]);
        assert!(opt_text(font_opts()[0]).starts_with("Pretendard"), "{}", opt_text(font_opts()[0]));
        assert_eq!(opt_text(font_opts()[3]), "맑은 고딕");
        let mut c = Config::default();
        assert_eq!(font_get(&c), 0);
        font_set(&mut c, 3);
        assert_eq!((c.appearance.font.as_str(), font_get(&c)), ("Malgun Gothic", 3));
        font_set(&mut c, 1);
        assert_eq!((c.appearance.font.as_str(), font_get(&c)), (crate::fonts::SYSTEM_CHOICE, 1));
        c.appearance.font = "Gone Sans".into();
        assert_eq!(font_get(&c), 0);
        // An older setting naming a Pretendard no longer installed: the
        // built-in default, as it is drawn.
        c.appearance.font = "Pretendard".into();
        assert_eq!(font_get(&c), 0);
        // The review (2026-11-11): an installed Pretendard is its own row,
        // and the choice goes there and back as that family, drawn from
        // the PC (not the built-in collection).
        assert_eq!(opt_text(font_opts()[4]), "Pretendard Variable");
        c.appearance.font = "Pretendard Variable".into();
        assert_eq!(font_get(&c), 4);
        font_set(&mut c, 0);
        assert_eq!((c.appearance.font.as_str(), font_get(&c)), ("", 0));
        font_set(&mut c, 4);
        assert_eq!((c.appearance.font.as_str(), font_get(&c)), ("Pretendard Variable", 4));
        let drawn = crate::fonts::pick(&c.appearance.font, crate::fonts::THEME_DEFAULT, "Segoe UI", |_| true);
        assert_eq!(drawn, "Pretendard Variable");
        assert!(crate::fonts::collection_for(&drawn).is_none(), "the installed one");
        let saved = crate::config::from_json(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!((saved.appearance.font.as_str(), font_get(&saved)), ("Pretendard Variable", 4));
        font_set(&mut c, 0);
        assert_eq!(c.appearance.font, "");
        let drawn = crate::fonts::pick(&c.appearance.font, crate::fonts::THEME_DEFAULT, "Segoe UI", |_| true);
        assert_eq!(drawn, crate::fonts::BUNDLED);
        assert!(crate::fonts::collection_for(&drawn).is_some(), "the built-in one");
        // The font travels by value, the size by its row.
        let mut from = Config::default();
        from.appearance.font = "Pretendard".into();
        from.appearance.text_size = "large".into();
        let mut to = Config::default();
        apply_keys(&from, &mut to, &["appearance.font".to_string(), "appearance.text_size".to_string()]);
        assert_eq!(to.appearance, from.appearance);
    }

    #[test]
    fn launchpad_library_rows_and_keys() {
        let secs = sections();
        let lp = page(&secs, "settings.tab.search");
        let at = |k: &str| lp.rows.iter().position(|r| r.key == k).unwrap();
        assert_eq!(at("launchpad.show_library"), at("group.search.launchpad") + 1, "first under Launchpad");
        assert!(!lp.rows.iter().any(|r| r.key == "action.launchpad_cleanup"), "no clean-up button: the banner has its own Not now");
        // Searched by other words too.
        assert!(crate::i18n::get("search.alias.launchpad.show_library").is_some());
        // The switch travels by its row; the banner by its key; Reset clears
        // the schema with Home (every app is Home again on the next open).
        let mut from = Config::default();
        from.launchpad.show_library = false;
        let mut to = Config::default();
        to.launchpad.hidden = vec!["h".into()];
        to.launchpad.cleanup_dismissed = true;
        to.launchpad.schema = 1;
        apply_keys(&from, &mut to, &["launchpad.show_library".to_string()]);
        assert!(!to.launchpad.show_library && to.launchpad.cleanup_dismissed);
        apply_keys(&from, &mut to, &["launchpad.cleanup_dismissed".to_string()]);
        assert!(!to.launchpad.cleanup_dismissed);
        apply_keys(&from, &mut to, &["launchpad.layout".to_string()]);
        assert_eq!((to.launchpad.schema, to.launchpad.hidden.len()), (0, 1), "hidden apps stay hidden");
    }

    #[test]
    fn search_page_holds_what_its_name_says() {
        let secs = sections();
        let keys = |t: &str| page(&secs, t).rows.iter().filter(|r| is_setting(r)).map(|r| r.key).collect::<Vec<_>>();
        assert_eq!(keys("settings.tab.search"), ["search.with", "launchpad.show_library"]);
        // What the dock shows, Launchpad and the Recycle Bin among it; the
        // island's rows in a card of their own on the Top bar page.
        let dock = page(&secs, "settings.section.dock");
        let shown: Vec<&str> = dock.rows.iter().skip_while(|r| r.key != ALL_DOCKS).skip(1).take(4).map(|r| r.key).collect();
        assert_eq!(shown, ["dock.indicator", LAUNCHPAD_PIN, "dock.show_trash", "dock.squircle"]);
        let bar = page(&secs, "settings.section.topbar");
        let island: Vec<&str> = bar.rows.iter().skip_while(|r| r.key != ISLAND_CARD).skip(1).take(2).map(|r| r.key).collect();
        assert_eq!(island, ["topbar.island", "topbar.island_screenshot"]);
        let items: Vec<&str> = bar.rows.iter().skip_while(|r| r.key != "group.topbar.items").skip(1).take_while(|r| r.key != ISLAND_CARD).map(|r| r.key).collect();
        assert_eq!(
            items,
            ["topbar.show_app_menus", "topbar.tray", "topbar.show_desktops", "input.indicator", "topbar.show_battery", "topbar.show_wifi", "topbar.show_volume"],
            "on by default first, then the readers"
        );
        // Quitting is not hidden under Advanced, and sits by the taskbar switch.
        let general = page(&secs, "settings.section.general");
        let at = |k: &str| general.rows.iter().position(|r| r.key == k).unwrap();
        assert_eq!(at("action.quit"), at("hide_taskbar") + 1);
        assert_eq!(fold_of(&general.rows, at("action.quit")), None);
        // The picked dock's heading comes right before its own rows.
        let dock = page(&secs, "settings.section.dock");
        let at = |k: &str| dock.rows.iter().position(|r| r.key == k).unwrap();
        assert_eq!(at(THIS_DOCK) + 1, at(SAME_AS_MAIN));
        assert!(DISPLAY_ROWS.iter().all(|k| at(k) > at(THIS_DOCK) && at(k) < at(ALL_DOCKS)));
    }

    #[test]
    fn autohide_delay_is_kept_in_range_and_travels() {
        let mut c = Config::default();
        assert_eq!(c.dock.autohide_delay(), 650, "the default");
        for (set, got) in [(0, 200), (199, 200), (200, 200), (900, 900), (2000, 2000), (60_000, 2000)] {
            c.dock.autohide_delay_ms = set;
            assert_eq!(c.dock.autohide_delay(), got, "{set}");
        }
        // Through the file: a value out of range is read and kept in range.
        let mut c = Config::default();
        c.dock.autohide_delay_ms = 1200;
        let back: Config = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back.dock.autohide_delay_ms, 1200);
        let old: Config = serde_json::from_str(r#"{"dock":{"autohide":true}}"#).unwrap();
        assert_eq!(old.dock.autohide_delay(), 650, "older files: the default");
        let wild: Config = serde_json::from_str(r#"{"dock":{"autohide_delay_ms":5}}"#).unwrap();
        assert_eq!(wild.dock.autohide_delay(), 200);
        // No row: every dock waits the default ("Normal").
        assert!(find_row(&sections(), "dock.autohide_delay_ms").is_none());
    }
}
