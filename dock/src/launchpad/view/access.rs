//! Launchpad for screen readers (crate::a11y): the search field, the
//! apps and folders on screen (a folder is a group that expands into its
//! panel), and in edit mode Done and "Move earlier / later" for the
//! selected icon. An open menu is all the tree shows (it is modal: what is behind it cannot be acted on). The
//! root's name is what was last said (the window's name), so the
//! announcements stay as they were. The "try again" button is there while
//! the app list could not be read (all of it, or not all).
//!
//! The App Library's tiles are groups named "Games, category, 12 apps"
//! that expand into the category's panel.
//!
//! Folders are named by their session id (LpItem::Folder::id), not by
//! where they are: a provider a client keeps follows its folder through
//! moves and renames, and goes when the folder does.

use windows::Win32::Foundation::RECT;
use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

use super::super::folders;
use super::super::grid::Rect;
use super::{Focus, Pad};
use crate::a11y::{self, Act, Node, Role, Tree};
use crate::config::LpItem;
use crate::i18n::{t, tf};
use crate::search::edit::Field;

/// Edit mode's buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Button {
    Done,
    MoveBack,
    MoveOn,
    FolderClose,
    /// Reads the app list again (it could not be read).
    Retry,
}

/// What a client's request comes to (pure: `route`).
#[derive(Clone, Debug, PartialEq)]
enum Route {
    /// A button: run it (Invoke only), or just put the focus on it.
    Button(Button, bool),
    Field,
    App(String),
    Folder(u64),
    FolderApp(String),
    /// A Library tile, by its category's key.
    LibTile(String),
    /// Nothing (behind the open menu, unknown, or not for this child).
    Blocked,
}

/// Where request `act` on child `key` goes. With a menu up (the shared
/// menu: a window and a tree of its own) nothing behind it acts; a button
/// runs on Invoke alone (SetFocus only moves the focus to it).
fn route(key: &str, act: Act, menu_open: bool) -> Route {
    if menu_open {
        return Route::Blocked;
    }
    let button = match key {
        "done" => Some(Button::Done),
        "move:back" => Some(Button::MoveBack),
        "move:on" => Some(Button::MoveOn),
        "folder:close" => Some(Button::FolderClose),
        "retry" => Some(Button::Retry),
        _ => None,
    };
    if let Some(b) = button {
        return match act {
            Act::Invoke => Route::Button(b, true),
            Act::Focus => Route::Button(b, false),
            _ => Route::Blocked,
        };
    }
    if key == "field" {
        Route::Field
    } else if let Some(t) = key.strip_prefix("app:") {
        Route::App(t.to_string())
    } else if let Some(id) = key.strip_prefix("folder:").and_then(|n| n.parse().ok()) {
        Route::Folder(id)
    } else if let Some(c) = key.strip_prefix("lib:") {
        Route::LibTile(c.to_string())
    } else if let Some(t) = key.strip_prefix("fapp:") {
        Route::FolderApp(t.to_string())
    } else {
        Route::Blocked
    }
}

/// The "try again" button where it is on screen (`at`; None: not shown):
/// named as it reads ("launch.action.retry"), what failed as its help
/// (`why`, a key).
fn retry_node(at: Option<RECT>, why: &str) -> Option<Node> {
    at.map(|r| Node::new("retry", Role::Button, t("launch.action.retry"), r).help(t(why)).invoke())
}

impl Pad {
    /// Publishes the tree when a screen reader is reading the window.
    pub(super) fn publish(&self) {
        if a11y::wanted(self.hwnd) {
            a11y::publish(self.hwnd, self.tree());
        }
    }

    fn tree(&self) -> Tree {
        let mut wr = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut wr);
        }
        let (origin, s) = ((wr.left, wr.top), self.s);
        let px = |r: Rect| a11y::px_rect(origin, s, r.l, r.t, r.w(), r.h());
        let name = if self.spoken.is_empty() { t("launchpad.title").to_string() } else { self.spoken.clone() };
        let placeholder = if self.arr.edit { t("launchpad.edit.hint") } else { t("launchpad.placeholder") };
        let mut nodes = vec![Node::new("field", Role::Edit, placeholder, px(self.field_rect())).value(self.query())];
        let mut focus = match self.focus {
            Focus::Field => Some("field".to_string()),
            Focus::Grid => None,
        };
        if let Some(f) = &self.arr.folder {
            let apps = self.folder_apps();
            let first = f.scroll * f.pg.cols;
            for slot in 0..f.pg.cols * f.pg.rows {
                let Some(app) = apps.get(first + slot).and_then(|id| self.app_by_id(id)) else { break };
                let r = f.pg.item(slot, self.rtl).offset(self.work.l, self.work.t);
                let mut n = Node::new(format!("fapp:{}", app.target()), Role::Button, self.app_said(app), px(r));
                if !self.arr.edit {
                    n = n.invoke().menu();
                }
                if f.sel == Some(first + slot) {
                    focus = Some(n.key.clone());
                }
                nodes.push(n);
            }
            let panel = folders::panel_rect(&f.pg).offset(self.work.l, self.work.t);
            let name = self.folder_name();
            nodes.push(Node::new("folder:close", Role::Button, tf("launchpad.folder.close", &[("name", &name)]), px(panel)).invoke());
        } else if self.on_lib() {
            for k in 0..self.lib.groups.len() {
                let r = self.lib.tiles.item(k, self.rtl).offset(self.work.l, self.work.t);
                let ((l, tp), (rr, b)) = (self.from_grid(r.l, r.t), self.from_grid(r.r, r.b));
                let n = Node::new(format!("lib:{}", self.lib.groups[k].cat.key()), Role::Group, self.tile_said(k), px(Rect::new(l, tp, rr, b))).expand(false).invoke().menu();
                if self.focus == Focus::Grid && self.lib.sel == Some(k) {
                    focus = Some(n.key.clone());
                }
                nodes.push(n);
            }
        } else {
            for slot in 0..self.per_page() {
                let Some(i) = self.index_of_slot(slot) else { continue };
                // As shown: edit mode scales the grid around its middle.
                let r = self.grid.item(slot, self.rtl).offset(self.work.l, self.work.t);
                let ((l, tp), (rr, b)) = (self.from_grid(r.l, r.t), self.from_grid(r.r, r.b));
                let r = Rect::new(l, tp, rr, b);
                let mut n = match (!self.searching).then(|| self.item_at(i)).flatten() {
                    Some(LpItem::Folder { name, apps, id, .. }) => {
                        let name = folders::shown_name(name, t("launchpad.folder.default"));
                        Node::new(format!("folder:{id}"), Role::Group, tf("launchpad.folder.a11y", &[("name", &name), ("count", &apps.len().to_string())]), px(r)).expand(false).invoke()
                    }
                    _ => {
                        let Some(app) = self.app_of(i) else { continue };
                        let mut n = Node::new(format!("app:{}", app.target()), Role::Button, self.app_said(app), px(r));
                        if self.arr.edit {
                            n = n.help(t("launchpad.edit.item_hint"));
                        } else {
                            n = n.invoke().menu();
                        }
                        n
                    }
                };
                // Edit mode: picked or not (Toggle; organize.rs), and the
                // picked ones' menu.
                if self.arr.edit && !self.searching {
                    n = n.toggle(self.is_picked(i)).menu();
                }
                if self.focus == Focus::Grid && self.sel == Some(i) {
                    focus = Some(n.key.clone());
                }
                nodes.push(n);
            }
        }
        nodes.extend(retry_node(self.msg_retry.map(px), self.retry_why));
        if self.arr.edit {
            let done = self.arr.done_rect;
            nodes.push(Node::new("done", Role::Button, t("launchpad.edit.done"), px(done)).invoke());
            if self.arr.folder.is_none() && !self.searching && self.sel.is_some_and(|i| self.valid_sel(i)) {
                nodes.push(Node::new("move:back", Role::Button, t("launchpad.edit.move_back"), px(done)).invoke());
                nodes.push(Node::new("move:on", Role::Button, t("launchpad.edit.move_on"), px(done)).invoke());
            }
        }
        // A button the client focused (Done, Move, Close, Try again) while
        // it is there.
        if let Some(k) = self.ax_focus.as_ref().filter(|k| nodes.iter().any(|n| &n.key == *k)) {
            focus = Some(k.clone());
        }
        Tree { role: Role::Group, name, nodes, focus, focusable: true, active: true, focus_in: None }
    }

    /// What a screen reader asked a child to do.
    pub(super) fn act(&mut self, key: &str, act: Act) {
        if act == Act::Refresh {
            self.publish();
            return;
        }
        if self.closing || self.done {
            return;
        }
        let r = route(key, act, self.menu_open);
        // A screen reader moves the focus; its ring shows only as it did,
        // or with Windows' keyboard cues always on (crate::input).
        let moves = matches!(act, Act::Focus | Act::Select);
        if moves {
            self.reader_ring = crate::input::reader_focus(self.by_key || self.field_ring || self.reader_ring);
            self.by_key = self.reader_ring;
            self.field_ring = self.focus == Focus::Field && self.reader_ring;
        }
        if !matches!(r, Route::Button(_, false)) {
            self.ax_focus = None;
            self.done_focus_changed();
        }
        match r {
            Route::Blocked => {}
            Route::Button(b, run) => {
                if !run {
                    self.ax_focus = Some(key.to_string());
                    // (Done shows the ring while the focus is on it: arrange.rs.)
                    self.done_focus_changed();
                } else {
                    match b {
                        Button::Done => self.end_edit(),
                        Button::MoveBack => self.arr_move_sel(false),
                        Button::MoveOn => self.arr_move_sel(true),
                        Button::FolderClose => self.close_folder(),
                        Button::Retry => self.retry_apps(),
                    }
                }
            }
            Route::Field => {
                if act == Act::SetValue {
                    if let Some(v) = a11y::take_value(self.hwnd, "field") {
                        self.arr_leave_for_typing();
                        self.field = Field::default();
                        self.field.insert(&v);
                        self.changed();
                    }
                }
                self.set_focus(Focus::Field);
            }
            Route::App(target) => {
                let Some(i) = self.index_of_target(&target) else { return };
                match act {
                    Act::Toggle if self.arr.edit && !self.searching => self.toggle_pick_at(i),
                    Act::Invoke => self.launch(i),
                    Act::Menu => {
                        let slot = if self.searching { i } else { i % self.per_page() };
                        let r = self.grid.icon(slot, self.rtl).offset(self.work.l, self.work.t);
                        self.open_menu(i, (r.cx(), r.cy()), true);
                    }
                    _ => {
                        self.focus = Focus::Grid;
                        self.select(i);
                    }
                }
            }
            Route::Folder(id) => {
                // Found by who it is, wherever it went (gone: nothing).
                let Some(i) = folders::find(&self.layout, id).map(|p| self.flat(p)) else { return };
                match act {
                    Act::Toggle if self.arr.edit => self.toggle_pick_at(i),
                    Act::Menu if self.arr.edit => {
                        let r = self.grid.icon(i % self.per_page(), self.rtl).offset(self.work.l, self.work.t);
                        self.open_menu(i, (r.cx(), r.cy()), true);
                    }
                    Act::Invoke | Act::Expand => self.open_folder(i),
                    Act::Collapse => self.close_folder(),
                    _ => {
                        self.focus = Focus::Grid;
                        self.select(i);
                    }
                }
            }
            Route::FolderApp(target) => {
                let apps = self.folder_apps();
                let Some(i) = apps.iter().position(|id| self.app_by_id(id).is_some_and(|a| a.target() == target)) else { return };
                match act {
                    Act::Invoke if !self.arr.edit => self.launch_from_folder(i),
                    Act::Focus => self.folder_select(i),
                    Act::Menu => {
                        let Some(f) = &self.arr.folder else { return };
                        let slot = i.saturating_sub(f.scroll * f.pg.cols);
                        let r = f.pg.icon(slot, self.rtl).offset(self.work.l, self.work.t);
                        self.panel_menu(i, (r.cx(), r.cy()), true);
                    }
                    _ => {}
                }
            }
            Route::LibTile(cat) => {
                // Found by its category, wherever it is now.
                let Some(k) = self.lib.groups.iter().position(|g| g.cat.key() == cat) else { return };
                match act {
                    Act::Invoke | Act::Expand => self.open_category(k),
                    Act::Collapse => self.close_folder(),
                    Act::Menu => {
                        let r = self.lib.tiles.tile(k, self.rtl).offset(self.work.l, self.work.t);
                        self.lib_menu(k, super::super::library::Part::Rest, (r.cx(), r.cy()), true);
                    }
                    _ => self.lib_select(k),
                }
            }
        }
        if moves && !self.reader_ring {
            // The focus moved without the ring.
            self.by_key = false;
            self.field_ring = false;
            self.update_sel();
            self.draw_field();
            if self.arr.folder.is_some() {
                self.draw_folder_sel(false);
            }
        }
        self.publish();
    }

    /// The listing index of the app launched as `target`, on screen.
    fn index_of_target(&self, target: &str) -> Option<usize> {
        (0..self.per_page()).filter_map(|s| self.index_of_slot(s)).find(|&i| self.app_of(i).is_some_and(|a| a.target() == target))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_run_on_invoke_only() {
        for (key, b) in [("done", Button::Done), ("move:back", Button::MoveBack), ("move:on", Button::MoveOn), ("folder:close", Button::FolderClose), ("retry", Button::Retry)] {
            assert_eq!(route(key, Act::Invoke, false), Route::Button(b, true), "{key}");
            assert_eq!(route(key, Act::Focus, false), Route::Button(b, false), "{key}: SetFocus only focuses");
            for other in [Act::Expand, Act::Collapse, Act::Toggle, Act::Menu, Act::SetValue] {
                assert_eq!(route(key, other, false), Route::Blocked, "{key}: {other:?}");
            }
        }
    }

    #[test]
    fn the_try_again_button_is_there_with_its_name_while_shown() {
        let _l = crate::i18n::test_lock("en");
        let at = RECT { left: 10, top: 20, right: 90, bottom: 48 };
        let n = retry_node(Some(at), super::super::FAILED_LINE).expect("shown: a node");
        assert_eq!((n.key.as_str(), n.role), ("retry", Role::Button));
        assert_eq!(n.name, t("launch.action.retry"));
        assert!(!n.name.is_empty() && n.name != "launch.action.retry", "a translated name");
        assert!(!n.help.is_empty() && n.help != super::super::FAILED_LINE, "what failed, translated");
        assert!(n.invoke, "Invoke runs it");
        assert_eq!(n.rect, at, "where it is drawn");
        // The list being read again (the ring) or read: no button, no node.
        assert!(retry_node(None, super::super::FAILED_LINE).is_none());
        // Invoke reads the list again; SetFocus only focuses it.
        assert_eq!(route("retry", Act::Invoke, false), Route::Button(Button::Retry, true));
        assert_eq!(route("retry", Act::Focus, false), Route::Button(Button::Retry, false));
        assert_eq!(route("retry", Act::Invoke, true), Route::Blocked, "behind a menu");
    }

    #[test]
    fn nothing_behind_an_open_menu_acts() {
        for key in ["app:shell:AppsFolder\\x", "folder:7", "done", "field", "power:1", "move:on", "card:2"] {
            assert_eq!(route(key, Act::Invoke, true), Route::Blocked, "{key} behind the menu");
        }
        assert_eq!(route("card:1", Act::Invoke, false), Route::Blocked, "no such child");
    }

    #[test]
    fn children_by_key() {
        assert_eq!(route("folder:42", Act::Expand, false), Route::Folder(42), "folders by session id");
        assert_eq!(route("folder:x", Act::Expand, false), Route::Blocked);
        assert_eq!(route("fapp:shell:AppsFolder\\a", Act::Focus, false), Route::FolderApp("shell:AppsFolder\\a".into()));
        assert_eq!(route("app:shell:AppsFolder\\a", Act::Invoke, false), Route::App("shell:AppsFolder\\a".into()));
        assert_eq!(route("power:4", Act::Invoke, false), Route::Blocked, "no power buttons");
        assert_eq!(route("field", Act::SetValue, false), Route::Field);
        assert_eq!(route("lib:games", Act::Expand, false), Route::LibTile("games".into()), "Library tiles by category");
        assert_eq!(route("lib:games", Act::Invoke, true), Route::Blocked, "behind a menu");
        // No banner and no clean-up panel any more.
        assert_eq!(route("banner:1", Act::Invoke, false), Route::Blocked);
        assert_eq!(route("clean:row:1", Act::Toggle, true), Route::Blocked);
    }
}
