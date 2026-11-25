//! The dock for screen readers (its UIA tree, see crate::a11y) and for the
//! keyboard: its focus shortcut ("focus_dock") puts
//! the focus on the dock; arrows move between icons, Enter / Space opens,
//! Shift+F10 or the Applications key shows the icon's menu, Esc gives the
//! focus back to the window that had it.
//!
//! The focused icon is shown the way the pointer would show it (magnified,
//! with its name), and the dock never takes the focus any other way.

use std::sync::Mutex;
use crate::hotkeys::Status;

use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, RegisterHotKey, SetFocus, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT, VK_APPS, VK_DOWN, VK_END, VK_ESCAPE, VK_F10, VK_HOME,
    VK_LEFT, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_UP,
};

use super::*;
use crate::a11y::{self, Act, AppState, Node, Role, Tree};

/// WM_HOTKEY id of the focus-the-dock hotkey (search 0x5345, Launchpad 0x4C50).
pub(super) const FOCUS_HOTKEY: i32 = 0x4644;

/// What is registered now: (owner, spec as given, registration result).
static HOTKEY: Mutex<Option<(isize, String, Status)>> = Mutex::new(None);

/// Registers `spec` as the focus-the-dock hotkey for `owner`, replacing the
/// one before. A spec that does not parse or that another app holds is
/// logged (the dock stays reachable by mouse and screen reader).
pub(super) fn apply_hotkey(owner: HWND, spec: &str) -> Status {
    let Ok(mut cur) = HOTKEY.lock() else { return Status::Failed };
    let me = owner.0 as isize;
    if let Some((_, _, status)) = cur.as_ref().filter(|(o, s, _)| *o == me && s == spec) {
        return *status;
    }
    if let Some((old, _, Status::Ok)) = cur.take() {
        unsafe {
            let _ = UnregisterHotKey(Some(HWND(old as *mut _)), FOCUS_HOTKEY);
        }
    }
    let status = match crate::search::hotkey::parse(spec) {
        Ok(None) => Status::Off,
        Ok(Some((mods, key))) => match unsafe { RegisterHotKey(Some(owner), FOCUS_HOTKEY, HOT_KEY_MODIFIERS(mods) | MOD_NOREPEAT, key) } {
            Ok(()) => {
                crate::log!("dock: focus hotkey {spec}");
                Status::Ok
            }
            Err(e) => {
                crate::log!("dock: focus hotkey {spec} not registered ({e})");
                Status::from_error(&e)
            }
        },
        Err(e) => {
            crate::log!("dock: bad focus hotkey {e}");
            Status::Invalid
        }
    };
    *cur = Some((me, spec.to_string(), status));
    status
}

/// Gives the hotkey back (the dock window is going away).
pub(super) fn release_hotkey() {
    let Ok(mut cur) = HOTKEY.lock() else { return };
    if let Some((owner, _, Status::Ok)) = cur.take() {
        unsafe {
            let _ = UnregisterHotKey(Some(HWND(owner as *mut _)), FOCUS_HOTKEY);
        }
    }
}

/// What a key press in keyboard mode asks for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum KeyMove {
    /// Focus this position in the list of icons.
    To(usize),
    Open,
    Menu,
    Leave,
    Nothing,
}

/// Pure: what `vk` does with the focus at `pos` of `n` icons. Up / Left
/// go towards the start, Down / Right towards the end (no wrap, like a
/// toolbar).
pub(super) fn key_move(vk: u16, shift: bool, pos: usize, n: usize) -> KeyMove {
    if n == 0 {
        return KeyMove::Leave;
    }
    let last = n - 1;
    match vk {
        v if v == VK_LEFT.0 || v == VK_UP.0 => KeyMove::To(pos.saturating_sub(1).min(last)),
        v if v == VK_RIGHT.0 || v == VK_DOWN.0 => KeyMove::To((pos + 1).min(last)),
        v if v == VK_HOME.0 => KeyMove::To(0),
        v if v == VK_END.0 => KeyMove::To(last),
        v if v == VK_RETURN.0 || v == VK_SPACE.0 => KeyMove::Open,
        v if v == VK_APPS.0 || (v == VK_F10.0 && shift) => KeyMove::Menu,
        v if v == VK_ESCAPE.0 => KeyMove::Leave,
        _ => KeyMove::Nothing,
    }
}

/// Pure: the items (kind, clickable) the keyboard stops at, in dock
/// order: dividers are not among them (the arrows go past).
pub(super) fn keyboard_stops(items: &[(Kind, bool)]) -> Vec<usize> {
    (0..items.len()).filter(|&i| items[i].1 && items[i].0 != Kind::Divider).collect()
}

/// Pure: the items UIA lists, in dock order: the keyboard's stops and the
/// dividers between them (as separators: read, never focused).
pub(super) fn a11y_listed(items: &[(Kind, bool)]) -> Vec<usize> {
    (0..items.len()).filter(|&i| items[i].1).collect()
}

/// Keys of the items, made unique ("#2" for a repeat) in item order.
pub(super) fn unique_keys(raw: Vec<String>) -> Vec<String> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    raw.into_iter()
        .map(|k| {
            let n = seen.entry(k.clone()).or_insert(0);
            *n += 1;
            if *n == 1 { k } else { format!("{k}#{n}") }
        })
        .collect()
}

impl Dock {
    fn raw_key(&self, i: usize) -> String {
        let it = &self.items[i];
        match it.kind {
            Kind::Start => "start".into(),
            Kind::Trash => "trash".into(),
            Kind::Sep => "sep".into(),
            Kind::Divider => format!("divider:{}", it.path.to_lowercase()),
            Kind::App => format!("app:{}", it.path.to_lowercase()),
        }
    }

    /// Every item's key (index-aligned with items).
    pub(super) fn item_keys(&self) -> Vec<String> {
        unique_keys((0..self.items.len()).map(|i| self.raw_key(i)).collect())
    }

    fn kinds(&self) -> Vec<(Kind, bool)> {
        self.items.iter().map(|it| (it.kind, it.clickable())).collect()
    }

    /// Items a screen reader / the keyboard can reach, in dock order.
    fn reachable(&self) -> Vec<usize> {
        keyboard_stops(&self.kinds())
    }

    fn a11y_items(&self) -> Vec<usize> {
        a11y_listed(&self.kinds())
    }

    fn item_of(&self, key: &str) -> Option<usize> {
        let keys = self.item_keys();
        self.reachable().into_iter().find(|&i| keys[i] == key)
    }

    /// What the dock shows, for UIA.
    pub(super) fn a11y_tree(&self) -> Tree {
        let keys = self.item_keys();
        let off = self.hidden || self.fullscreen;
        let nodes = self
            .a11y_items()
            .into_iter()
            .map(|i| {
                let it = &self.items[i];
                let rect = if off { RECT::default() } else { self.icon_rect(i) };
                if it.kind == Kind::Divider {
                    return Node::new(keys[i].clone(), Role::Separator, crate::i18n::t("dock.item.divider"), rect);
                }
                let mut n = Node::new(keys[i].clone(), Role::Button, it.name.clone(), rect).invoke().menu();
                if it.kind == Kind::App {
                    let badge = match &it.marks.mark.badge {
                        badges::Badge::Count(c) => Some(c.clone()),
                        badges::Badge::Dot => Some(String::new()),
                        badges::Badge::None | badges::Badge::Hung | badges::Badge::Tray => None,
                    };
                    n = n.help(a11y::app_help(&AppState {
                        windows: it.windows.len(),
                        hung: it.hung,
                        badge,
                        progress: it.marks.mark.progress,
                        stack: it.stack,
                    }));
                    if it.stack {
                        n = n.expand(stack::is_open() && self.last_stack.eq_ignore_ascii_case(&it.path));
                    }
                }
                n
            })
            .collect();
        // Choosing one of an app's windows: its cards are in the tree, the
        // chosen one has the focus (a screen reader reads its full title).
        let mut nodes: Vec<Node> = nodes;
        let mut focus = self.kb.clone();
        // Each card's buttons follow it (close, always on top as a toggle,
        // next monitor, minimize / restore); unavailable ones are disabled.
        for card in preview::kb_cards() {
            let key = preview::card_key(card.window);
            if card.chosen {
                focus = Some(key.clone());
            }
            nodes.push(Node::new(key, Role::MenuItem, card.title, card.rect).help(card.help).invoke());
            for b in card.buttons {
                if b.focused {
                    focus = Some(b.key.clone());
                }
                let n = Node::new(b.key, Role::Button, b.name, b.rect).enabled(b.enabled);
                nodes.push(match b.on {
                    Some(on) => n.toggle(on),
                    None => n.invoke(),
                });
            }
        }
        // Focusable: SetFocus from a client takes the focus (keyboard mode).
        Tree {
            role: Role::ToolBar,
            name: crate::i18n::t("a11y.dock").into(),
            nodes,
            focus,
            focusable: true,
            active: self.kb.is_some(),
            focus_in: None,
        }
    }

    /// The bar's screen rect (physical px), as last laid out.
    pub(super) fn bar_screen_rect(&self) -> RECT {
        let w = self.frame.to_win(self.bar_rect);
        a11y::px_rect((self.origin.x, self.origin.y), self.scale, w.x, w.y, w.w, w.h)
    }

    /// The icon's screen rect (physical px), as last laid out.
    pub(super) fn icon_rect(&self, i: usize) -> RECT {
        let it = &self.items[i];
        let baseline = self.bar_rect.y + PAD_CROSS + self.look.icon;
        let r = if it.kind == Kind::Divider {
            // Its line, not an icon's square (dividers::divider_box).
            super::dividers::divider_box(it.dx, it.w, baseline, self.look.icon)
        } else {
            Rect { x: it.dx, y: baseline - it.w, w: it.w, h: it.w }
        };
        let w = self.frame.to_win(r);
        a11y::px_rect((self.origin.x, self.origin.y), self.scale, w.x, w.y, w.w, w.h)
    }

    /// Icon `i`'s screen rect (physical px) as far out as it goes now: a
    /// bouncing icon (launch) at its highest point, it may still go up.
    pub(super) fn standing_rect(&self, i: usize) -> RECT {
        let r = self.icon_rect(i);
        if self.items[i].bounce.is_none() {
            return r;
        }
        let lift = (self.look.icon * 0.35 * self.scale).ceil() as i32;
        match self.frame.pos {
            Position::Bottom => RECT { top: r.top - lift, ..r },
            Position::Left => RECT { right: r.right + lift, ..r },
            Position::Right => RECT { left: r.left - lift, ..r },
        }
    }

    /// Icon `i`'s box at full magnification (physical px), on its baseline
    /// and centered where it is now: the farthest a hovered icon stands
    /// out of the bar (its own size with magnification off).
    pub(super) fn peak_rect(&self, i: usize) -> RECT {
        let it = &self.items[i];
        let side = self.look.icon * self.look.max_scale;
        let baseline = self.bar_rect.y + PAD_CROSS + self.look.icon;
        let w = self.frame.to_win(Rect { x: it.dx + it.w / 2.0 - side / 2.0, y: baseline - side, w: side, h: side });
        a11y::px_rect((self.origin.x, self.origin.y), self.scale, w.x, w.y, w.w, w.h)
    }

    /// Where a menu or notice for icon `i` opens from (screen px): the
    /// popup gap off the dock's outer side, past the bar and the icon
    /// (menu::menu_point), over the icon's middle.
    pub(super) fn menu_anchor(&self, i: usize) -> POINT {
        let gap = crate::placement::popup_gap(self.scale);
        super::menu::menu_point(self.frame.pos, self.bar_screen_rect(), Some(self.standing_rect(i)), POINT::default(), gap)
    }

    /// How a menu or notice sits against `menu_anchor` (away from the edge).
    pub(super) fn menu_align(&self) -> crate::popup::Align {
        match self.frame.pos {
            Position::Bottom => crate::popup::Align::Above,
            Position::Left => crate::popup::Align::RightOf,
            Position::Right => crate::popup::Align::LeftOf,
        }
    }

    /// Hands the tree to UIA while a client reads the dock (free
    /// otherwise; in keyboard mode too, so focus moves are heard at once).
    pub(super) fn publish_a11y(&self) {
        if self.kb.is_some() || a11y::wanted(self.hwnd) {
            a11y::publish(self.hwnd, self.a11y_tree());
        }
    }

    /// A UIA client's request. Some(item) asks the caller for that item's
    /// menu (shown outside the dock's borrow).
    pub(super) unsafe fn a11y_act(&mut self, key: &str, act: Act) -> Option<usize> {
        if act == Act::Refresh {
            self.publish_a11y();
            return None;
        }
        // A chooser card's button: its action (the chooser stays).
        if key.starts_with("win:") && matches!(act, Act::Invoke | Act::Toggle) && preview::press(key) {
            self.publish_a11y();
            return None;
        }
        // A client moves the focus to a card or one of its buttons (Enter
        // / Space then act on it, see preview::kb_key).
        if key.starts_with("win:") && act == Act::Focus && preview::kb_focus(key) {
            self.publish_a11y();
            return None;
        }
        // A chooser card: switch to its window.
        if let Some(h) = preview::kb_cards().into_iter().map(|c| c.window).find(|h| preview::card_key(*h) == key) {
            if act == Act::Invoke {
                preview::hide();
                self.leave_keyboard(false);
                super::input::focus(h);
            }
            return None;
        }
        let i = self.item_of(key)?;
        match act {
            Act::Invoke => {
                preview::hide();
                self.activate(i);
            }
            Act::Expand if self.items[i].stack => self.open_stack(i),
            Act::Collapse => stack::cancel(),
            Act::Menu => return Some(i),
            Act::Focus if self.kb.is_some() => self.kb_focus(i),
            // A client moves the focus to an icon: keyboard mode on it, the
            // ring only with Windows' keyboard cues always on.
            Act::Focus => self.enter_keyboard(Some(key), crate::input::reader_focus(false)),
            _ => {}
        }
        None
    }

    // ---- keyboard mode ------------------------------------------------------------

    /// Takes the keyboard focus, on the icon with `key` if it is still
    /// there (after its menu), else on the first; its ring shown with
    /// `ring` (crate::input).
    pub(super) unsafe fn enter_keyboard(&mut self, key: Option<&str>, ring: bool) {
        if self.fullscreen {
            return;
        }
        let list = self.reachable();
        let Some(&first) = list.first() else { return };
        let i = key.and_then(|k| self.item_of(k)).unwrap_or(first);
        // Remember who had the focus (not one of our own menus).
        let fg = GetForegroundWindow();
        let mut pid = 0u32;
        GetWindowThreadProcessId(fg, Some(&mut pid));
        if !fg.is_invalid() && pid != std::process::id() {
            self.kb_prev = fg;
        }
        if self.hidden {
            let _ = KillTimer(Some(self.hwnd), AUTOHIDE_TIMER);
            self.set_hidden(false);
        }
        let _ = SetForegroundWindow(self.hwnd);
        let _ = SetFocus(Some(self.hwnd));
        if GetForegroundWindow() != self.hwnd {
            crate::log!("dock: could not take the keyboard focus");
            return;
        }
        crate::log!("dock: keyboard mode");
        self.kb_ring = ring;
        self.kb_focus(i);
    }

    /// Windows taskbar traversal wraps and remembers the previous foreground window.
    pub(super) unsafe fn taskbar_cycle(&mut self, backwards: bool) {
        let stops = self.reachable();
        if stops.is_empty() { return; }
        let current = self.kb.as_ref().and_then(|k| self.item_of(k)).and_then(|i| stops.iter().position(|s| *s == i));
        let n = stops.len();
        let pos = match (current, backwards) {
            (Some(i), true) => (i+n-1)%n,
            (Some(i), false) => (i+1)%n,
            (None, true) => n-1,
            (None, false) => 0,
        };
        let key = self.item_keys()[stops[pos]].clone();
        self.enter_keyboard(Some(&key), true);
    }

    /// Moves the keyboard focus to item `i`: shown as if the pointer rested
    /// on it (magnified, named at once).
    fn kb_focus(&mut self, i: usize) {
        self.kb = Some(self.item_keys()[i].clone());
        let u = self.base_center(i);
        let v = self.bar_rect.y + self.bar_rect.h / 2.0;
        self.mouse = Some((u, v));
        self.label_armed = true;
        self.apply_region();
        self.update_targets();
        self.layout();
    }

    /// Leaves keyboard mode; `restore` gives the focus back to the window
    /// that had it.
    pub(super) unsafe fn leave_keyboard(&mut self, restore: bool) {
        if self.kb.take().is_none() {
            return;
        }
        self.kb_ring = false;
        crate::log!("dock: keyboard mode off");
        // The pointer may be on the dock for real: then it keeps it.
        if !self.cursor_over_window() {
            self.mouse = None;
            self.reset_label();
            if self.cfg.dock.autohide {
                SetTimer(Some(self.hwnd), AUTOHIDE_TIMER, self.cfg.dock.autohide_delay(), None);
            }
        }
        self.update_targets();
        self.layout();
        // (Kept when the focus just moved to our own menu: Esc after it
        // still goes back there.)
        if restore {
            let prev = std::mem::take(&mut self.kb_prev);
            if IsWindow(Some(prev)).as_bool() {
                let _ = SetForegroundWindow(prev);
            }
        }
    }

    /// A key in keyboard mode. Some(item): show its menu (outside the borrow).
    pub(super) unsafe fn kb_key(&mut self, vk: u16) -> Option<usize> {
        // Choosing one of an app's windows: the cards take the keys.
        if preview::kb_active() {
            if let Some(w) = preview::kb_key(vk) {
                self.leave_keyboard(false);
                super::input::focus(w);
            }
            // The chosen card (or the icon again, after Esc) is announced.
            self.publish_a11y();
            return None;
        }
        let list = self.reachable();
        let cur = self.kb.clone().and_then(|k| self.item_of(&k));
        // The focused icon went away: the nearest one keeps the focus.
        let pos = cur.and_then(|c| list.iter().position(|&i| i == c)).unwrap_or(0);
        let shift = GetKeyState(VK_SHIFT.0 as i32) < 0;
        // The ring shows once the keyboard moves the focus (crate::input).
        if crate::input::navigates(vk) && !self.kb_ring {
            self.kb_ring = true;
            self.place_rings();
        }
        match key_move(vk, shift, pos, list.len()) {
            KeyMove::To(p) => self.kb_focus(list[p]),
            KeyMove::Open => {
                let i = list[pos];
                // Several windows: the same cards as a click, chosen with
                // the arrows (Enter picks, Esc goes back to the icons).
                let it = &self.items[i];
                if it.kind == Kind::App && !it.stack && it.windows.len() > 1 {
                    self.show_preview(i);
                    if preview::open_for().as_deref() == Some(self.items[i].path.as_str()) {
                        // The window that was in front (before the dock took
                        // the keys) starts on the next one, like Ctrl+click
                        // cycling does.
                        let prev = self.kb_prev;
                        let wins = preview::windows();
                        let first = wins.iter().position(|&h| h == prev).map(|p| (p + 1) % wins.len()).unwrap_or(0);
                        preview::kb_start(first);
                        self.publish_a11y();
                        return None;
                    }
                }
                // The app (or stack, or Launchpad) takes the focus from here.
                self.leave_keyboard(false);
                preview::hide();
                self.activate(i);
            }
            KeyMove::Menu => return Some(list[pos]),
            KeyMove::Leave => self.leave_keyboard(true),
            KeyMove::Nothing => {}
        }
        None
    }

    /// The key of item `i` (to come back to it after its menu).
    pub(super) fn key_of(&self, i: usize) -> Option<String> {
        self.item_keys().get(i).cloned()
    }
}

#[cfg(test)]
mod tests {
    /// Keyboard mode's ring (crate::input): the dock's focus shortcut and
    /// the keys that move the focus show it; a media, volume or letter key
    /// moves nothing; a screen reader's focus enters keyboard mode with the
    /// ring only with Windows' keyboard cues always on; a press of the
    /// pointer ends keyboard mode.
    #[test]
    fn the_rings_rule_in_keyboard_mode() {
        for vk in crate::input::sample::QUIET_OPEN {
            assert_eq!(key_move(vk, false, 2, 5), KeyMove::Nothing, "{vk:#x}");
        }
        // (Tab is the dock's own: it leaves keyboard mode's toolbar; Page
        // Up / Down do nothing on a toolbar.)
        for vk in crate::input::NAVIGATION.into_iter().filter(|&v| ![0x09, 0x21, 0x22].contains(&v)) {
            assert!(matches!(key_move(vk, false, 2, 5), KeyMove::To(_) | KeyMove::Open), "{vk:#x}");
        }
        let src = include_str!("access.rs").replace("\r\n", "\n");
        let code = &src[..src.find("#[cfg(test)]").unwrap()];
        assert!(code.contains("if crate::input::navigates(vk) && !self.kb_ring {"));
        assert!(code.contains("Act::Focus => self.enter_keyboard(Some(key), crate::input::reader_focus(false)),"));
        assert!(include_str!("ring.rs").contains("let key = self.kb.as_ref().filter(|_| self.kb_ring)?;"), "the ring only with kb_ring");
        assert!(include_str!("input.rs").replace("\r\n", "\n").contains("if self.kb.is_some() {\n            unsafe { self.leave_keyboard(false) };"), "a press ends keyboard mode");
        assert!(include_str!("mod.rs").contains("d.enter_keyboard(None, true)"), "the focus shortcut shows the ring");
    }

    use super::*;

    #[test]
    fn arrows_move_within_the_dock() {
        let (l, r) = (VK_LEFT.0, VK_RIGHT.0);
        assert_eq!(key_move(r, false, 0, 3), KeyMove::To(1));
        assert_eq!(key_move(r, false, 2, 3), KeyMove::To(2), "no wrap at the end");
        assert_eq!(key_move(l, false, 0, 3), KeyMove::To(0), "no wrap at the start");
        assert_eq!(key_move(VK_DOWN.0, false, 1, 3), KeyMove::To(2), "side docks: down is next");
        assert_eq!(key_move(VK_UP.0, false, 1, 3), KeyMove::To(0));
        assert_eq!(key_move(VK_END.0, false, 0, 5), KeyMove::To(4));
        assert_eq!(key_move(VK_HOME.0, false, 4, 5), KeyMove::To(0));
        // A focus past the end (an icon went away) comes back inside.
        assert_eq!(key_move(l, false, 9, 3), KeyMove::To(2));
    }

    #[test]
    fn keys_open_menu_and_leave() {
        assert_eq!(key_move(VK_RETURN.0, false, 0, 3), KeyMove::Open);
        assert_eq!(key_move(VK_SPACE.0, false, 0, 3), KeyMove::Open);
        assert_eq!(key_move(VK_F10.0, true, 0, 3), KeyMove::Menu);
        assert_eq!(key_move(VK_F10.0, false, 0, 3), KeyMove::Nothing, "F10 alone is not the menu");
        assert_eq!(key_move(VK_APPS.0, false, 0, 3), KeyMove::Menu);
        assert_eq!(key_move(VK_ESCAPE.0, false, 0, 3), KeyMove::Leave);
        assert_eq!(key_move(VK_RIGHT.0, false, 0, 0), KeyMove::Leave, "an empty dock lets go");
    }

    #[test]
    fn repeated_keys_are_made_unique() {
        let k = |v: &[&str]| unique_keys(v.iter().map(|s| s.to_string()).collect());
        assert_eq!(k(&["start", "app:a", "app:a", "trash", "app:a"]), ["start", "app:a", "app:a#2", "trash", "app:a#3"]);
    }
}
