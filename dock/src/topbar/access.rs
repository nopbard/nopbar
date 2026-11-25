//! The top bar for screen readers (its UIA tree, see crate::a11y): a menu
//! bar with the logo, the app's menus, then the right side in reading order
//! (desktops, tray icons, status, Quick settings, clock).

use super::*;
use crate::a11y::{self, Act, Node, Role, Tree};

impl TopBar {
    pub(super) fn slot_key(&self, i: usize) -> String {
        match self.slots[i].cell {
            Cell::Start => "start".into(),
            Cell::App => "app".into(),
            Cell::Menu(k) => format!("menu:{k}"),
            Cell::Tray(k) => match (self.tray_ids.get(k), self.tray.get(k)) {
                (Some(id), _) => format!("tray:{id}"),
                (None, Some(e)) => format!("tray:{}:{}", e.owner, e.uid),
                (None, None) => format!("tray:#{k}"),
            },
            Cell::Chevron => "chevron".into(),
            Cell::TrayButton => "tray_button".into(),
            Cell::Net => "net".into(),
            Cell::Volume => "volume".into(),
            Cell::Battery => "battery".into(),
            Cell::Companion => "companion".into(),
            Cell::Search => "search".into(),
            Cell::Control => "control".into(),
            Cell::Clock => "clock".into(),
            Cell::Desktops => "desktops".into(),
            Cell::Hidden => "hidden".into(),
            Cell::Input => "input".into(),
        }
    }

    /// The slot with `key`, if it is on show.
    pub(super) fn slot_by_key(&self, key: &str) -> Option<usize> {
        (0..self.slots.len()).find(|&i| self.slots[i].w > 0.0 && self.slot_key(i) == key)
    }

    fn node(&self, i: usize) -> Option<Node> {
        use crate::i18n::{t, tf};
        let s = &self.slots[i];
        let key = self.slot_key(i);
        let rect = self.slot_rect(i);
        // Cells without a menu of their own have the bar's (right_click):
        // a screen reader's context menu key opens it.
        let bar_menu = right_click(Some(s.cell)) == RightClick::BarMenu;
        let button = |name: String| {
            let n = Node::new(key.clone(), Role::Button, name, rect).invoke();
            if bar_menu { n.menu() } else { n }
        };
        Some(match s.cell {
            // Same label and actions as the dock's Windows Start button.
            Cell::Start => button(t("a11y.topbar.start").into()).help(t("a11y.topbar.start.help")).menu(),
            Cell::App => Node::new(key.clone(), Role::MenuItem, self.app.clone(), rect).invoke().expand(self.open == Some(0)),
            Cell::Menu(k) => {
                let title = a11y::strip_mnemonic(self.titles.get(k)?);
                Node::new(key.clone(), Role::MenuItem, title, rect).invoke().expand(self.open == Some(k + 1))
            }
            Cell::Tray(k) => {
                let tip = self.tray.get(k).map(|e| e.tip.lines().next().unwrap_or("").trim().to_string()).unwrap_or_default();
                let name = if tip.is_empty() { t("a11y.topbar.tray_icon").to_string() } else { tip };
                button(name).menu()
            }
            Cell::Chevron => button(t("a11y.topbar.hidden_icons").into()).expand(self.expanded),
            Cell::TrayButton => button(t("topbar.tray.button").into()).help(t("topbar.tray.button.hint")),
            Cell::Net => button(t("a11y.topbar.network").into()),
            Cell::Volume => {
                let (p, muted) = self.volume?;
                let v = if muted { t("control.muted").to_string() } else { tf("control.percent", &[("p", &p.to_string())]) };
                button(t("control.sound").into()).value(v)
            }
            Cell::Battery => {
                let (p, charging) = self.battery?;
                let name = a11y::battery_name(p, charging);
                button(name.clone()).value(name)
            }
            Cell::Companion => button("NopBar".into()).menu(),
            Cell::Search => button(t("search.title").into()),
            // The dot says removable storage is connected; so does this.
            Cell::Control if self.cell_glyph(Cell::Control) == Some(crate::glyphs::Glyph::SlidersDot) => button(t("control.title").into()).help(t("control.removable.connected")),
            Cell::Control => button(t("control.title").into()),
            // What it shows: with the seconds while the bar shows them (the
            // tree is published on each tick only while a client listens,
            // and then only the name changes).
            Cell::Clock => {
                let now = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
                button(status::clock_name(&now, self.cfg.clock_seconds))
            }
            Cell::Desktops => {
                let d = &self.desktops;
                let cur = d.current.unwrap_or(0);
                let v = tf("topbar.desktops.position", &[("name", &d.name(cur)), ("i", &(cur + 1).to_string()), ("n", &d.count().to_string())]);
                button(t("a11y.topbar.desktops").into()).value(v).menu()
            }
            // Windows in the tray: Invoke shows one (lists several), the
            // menu lists them with Show and Close.
            Cell::Hidden => button(self.hidden_name()).menu(),
            // "Input: Hangul, Caps Lock on"; Invoke switches, the menu
            // lists the layouts.
            Cell::Input => button(self.input_name()).menu(),
        })
    }

    /// What the bar shows, for UIA (cells on show, in reading order).
    pub(super) fn a11y_tree(&self) -> Tree {
        let mut nodes: Vec<Node> = if self.hidden_full {
            Vec::new()
        } else {
            (0..self.slots.len()).filter(|&i| self.slots[i].w > 0.0).filter_map(|i| self.node(i)).chain(self.island_node()).collect()
        };
        a11y::reading_order(&mut nodes);
        let focus = self.tray_focus.as_ref().map(|(key, _)| key.clone());
        if focus.is_some() {
            let keys: Vec<_> = self.tray_stops().into_iter().map(|i| self.slot_key(i)).collect();
            for node in &mut nodes { node.unfocusable = !keys.contains(&node.key); }
        }
        Tree { active: focus.is_some(), focusable: focus.is_some(), focus, role: Role::MenuBar, name: crate::i18n::t("a11y.topbar").into(), nodes, ..Default::default() }
    }

    /// Hands the tree to UIA once a client has asked for it (free otherwise).
    pub(super) fn publish_a11y(&self) {
        if a11y::wanted(self.hwnd) {
            a11y::publish(self.hwnd, self.a11y_tree());
        }
    }
}

/// What a UIA request on the island does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IslandAct {
    Click,
    Expand(bool),
    /// The bar's menu (right_click: the island has none of its own).
    BarMenu,
    None,
}

/// Pure: the island's answer to `act`.
fn island_act(act: Act) -> IslandAct {
    match act {
        Act::Invoke | Act::Toggle => IslandAct::Click,
        Act::Expand => IslandAct::Expand(true),
        Act::Collapse => IslandAct::Expand(false),
        Act::Menu if right_click(None) == RightClick::BarMenu => IslandAct::BarMenu,
        _ => IslandAct::None,
    }
}

/// Pure: the user's next command on a bar calls off a waiting Start
/// request (keys::start_menu): they moved on, and Start must not open
/// late into it. `cell`: what it is on (None: the island, or nothing);
/// `act`: as a UIA client asks it, a click being Invoke and a right click
/// Menu. Only another Start invocation (the same request) and what is
/// no command (a focus move, a refresh) leave it.
pub(super) fn calls_off_start(cell: Option<Cell>, act: Act) -> bool {
    !matches!((cell, act), (_, Act::Focus | Act::Refresh) | (Some(Cell::Start), Act::Invoke))
}

/// The user's command `act` on `cell` (calls_off_start).
pub(super) fn command(cell: Option<Cell>, act: Act) {
    if calls_off_start(cell, act) {
        crate::keys::cancel_start(None);
    }
}

/// A UIA client's request for one of bar `hwnd`'s cells. Runs outside the
/// bars' borrow (menus and panels pump messages).
pub(super) fn act(hwnd: HWND, key: &str, act: Act) {
    if act == Act::Refresh {
        with_bar_of(hwnd, |b| b.publish_a11y());
        return;
    }
    let on = if key == "island" { None } else { with_bar_of(hwnd, |b| b.slot_by_key(key).map(|i| b.slots[i].cell)).flatten() };
    command(on, act);
    // The Live Island: Invoke clicks it (the card takes the keyboard); its
    // context menu is the bar's, as a right-click on it.
    if key == "island" {
        match island_act(act) {
            IslandAct::BarMenu => {
                // The card folds first; the menu hangs from the pill (up
                // from a bar at the bottom), outside the bars' borrow.
                let at = with_bar_of(hwnd, |b| {
                    b.island_event(super::island::model::Event::Dismiss);
                    b.island_screen_rect().map(|r| b.menu_at(&r))
                })
                .flatten();
                if let Some(at) = at {
                    open_bar_menu(hwnd, at);
                }
            }
            IslandAct::Click => {
                with_bar_of(hwnd, |b| b.island_click());
            }
            IslandAct::Expand(open) => {
                with_bar_of(hwnd, |b| b.island_expand(open));
            }
            IslandAct::None => {}
        }
        return;
    }
    let Some((i, cell)) = with_bar_of(hwnd, |b| b.slot_by_key(key).map(|i| (i, b.slots[i].cell))).flatten() else { return };
    match (cell, act) {
        (Cell::Tray(_) | Cell::TrayButton | Cell::Chevron, Act::Focus) => {
            with_bar_of(hwnd, |b| {
                if let Some((focused, _)) = b.tray_focus.as_mut() {
                    *focused = key.to_string();
                    b.layout_hover();
                    b.publish_a11y();
                }
            });
        }
        (Cell::App | Cell::Menu(_), Act::Invoke | Act::Expand) => {
            if let Some(t) = with_bar_of(hwnd, |b| b.title_of_slot(i)).flatten() {
                open_menus(hwnd, t);
            }
        }
        (Cell::App | Cell::Menu(_), Act::Collapse) => crate::popup::cancel(),
        (Cell::Chevron, Act::Invoke | Act::Toggle) => {
            with_bar_of(hwnd, |b| b.set_expanded(!b.expanded));
        }
        (Cell::Chevron, Act::Expand | Act::Collapse) => {
            with_bar_of(hwnd, |b| b.set_expanded(act == Act::Expand));
        }
        // The list of desktops (a menu) rather than a switch to one dot.
        (Cell::Desktops, Act::Invoke | Act::Menu) => open_panel(hwnd, i, panels::Panel::Desktops),
        (Cell::Input, Act::Menu) => open_panel(hwnd, i, panels::Panel::Input),
        (Cell::Hidden, Act::Menu) => hidden::menu(hwnd, i),
        (Cell::Companion, Act::Menu) => companion::click(hwnd, i, true, true),
        // Matches the mouse: Invoke opens Start; Menu opens Win+X.
        (Cell::Start, Act::Menu) => crate::keys::win_x(),
        (Cell::Tray(_), Act::Menu) => {
            if let Some((owner, uid, kind, at)) = with_bar_of(hwnd, |b| b.tray_target(i, true)).flatten() {
                crate::tray::click(owner, uid, kind, at);
            }
        }
        (_, Act::Invoke) => click_slot(hwnd, i, None),
        // The bar's own menu, hanging from the cell like its panels.
        (c, Act::Menu) if right_click(Some(c)) == RightClick::BarMenu => {
            if let Some(at) = with_bar_of(hwnd, |b| b.menu_at(&b.slot_rect(i))) {
                open_bar_menu(hwnd, at);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_islands_context_menu_is_the_bars() {
        assert_eq!(island_act(Act::Menu), IslandAct::BarMenu);
        assert_eq!(island_act(Act::Invoke), IslandAct::Click);
        assert_eq!(island_act(Act::Toggle), IslandAct::Click);
        assert_eq!(island_act(Act::Expand), IslandAct::Expand(true));
        assert_eq!(island_act(Act::Collapse), IslandAct::Expand(false));
        assert_eq!(island_act(Act::Focus), IslandAct::None);
    }

    #[test]
    fn another_command_calls_off_a_waiting_start_request() {
        // Shift held after Shift+right-click on Start, then the chevron
        // (a click or a screen reader's expand): Start never opens late.
        for act in [Act::Invoke, Act::Toggle, Act::Expand, Act::Collapse] {
            assert!(calls_off_start(Some(Cell::Chevron), act), "{act:?}");
        }
        assert!(calls_off_start(Some(Cell::Tray(0)), Act::Menu));
        assert!(calls_off_start(Some(Cell::Clock), Act::Invoke));
        assert!(calls_off_start(None, Act::Invoke), "the island");
        assert!(calls_off_start(Some(Cell::Start), Act::Menu), "Win+X cancels a pending Start");
        // The same request again, or no command at all: it waits on.
        assert!(!calls_off_start(Some(Cell::Start), Act::Invoke));
        assert!(!calls_off_start(Some(Cell::Chevron), Act::Focus));
        assert!(!calls_off_start(None, Act::Refresh));
    }
}
