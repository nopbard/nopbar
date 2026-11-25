//! Dividers between pinned apps: added from the dock's menu (at the end of
//! the pins), moved and dragged off like a pin (drag.rs), removed from
//! their own menu. Each is an entry of the pinned list
//! (config::PinEntry::divider) drawn like the separator.
//!
//! None stays redundant: one at the start of the pins, or right after
//! another, goes when the pins are saved (never while dragging). One at
//! the end stays: it sets the pinned apps off from the running ones.

use super::*;
use crate::config::normalize_dividers;

impl Dock {
    /// Puts a divider at the end of the pinned apps (one there already,
    /// or no app pinned: nothing to set apart, no twin).
    pub(super) fn add_divider(&mut self) {
        let shown: Vec<PinEntry> = self.items.iter().filter(|it| it.pin_slot()).map(|it| self.pin_entry(it)).collect();
        if !can_add(&shown) {
            return;
        }
        let entry = PinEntry::divider(&self.cfg.dock.pins);
        self.insert_pin_items(shown.len(), std::slice::from_ref(&entry), false);
        crate::log!("dock: divider {:?} added after the pins", entry.path);
        self.update_config(move |c| c.dock.pins = with_divider(&c.dock.pins, &entry));
        self.sync_windows();
        self.kick();
    }

    /// Takes divider `path` off the dock (its menu). Ctrl+Z puts it back,
    /// as after dragging it off (drag::remember_removal).
    pub(super) fn remove_divider(&mut self, path: &str) {
        let Some(i) = self.items.iter().position(|it| it.kind == Kind::Divider && it.path == path) else { return };
        let before: Vec<String> = self.items.iter().filter(|it| it.pin_slot()).map(|it| it.path.clone()).collect();
        let index = before.iter().position(|p| p == path).unwrap_or(before.len());
        let it = self.items.remove(i);
        self.poof(it);
        let dropped = self.drop_redundant_dividers();
        let also = went_along(&before, &dropped);
        let mut gone = dropped;
        gone.push(path.to_string());
        self.cfg.dock.pins = without(&self.cfg.dock.pins, &gone);
        self.update_config(move |c| c.dock.pins = without(&c.dock.pins, &gone));
        self.sync_windows();
        self.kick();
        crate::log!("dock: divider {path:?} removed from its menu (was #{index}); Ctrl+Z on the dock puts it back");
        self.remember_removal(PinEntry::new(String::new(), path.to_string()), index, also);
    }

    /// Takes redundant dividers off the dock (config::redundant_dividers
    /// over the pinned items); returns their paths, for the save.
    pub(super) fn drop_redundant_dividers(&mut self) -> Vec<String> {
        let slots: Vec<usize> = (0..self.items.len()).filter(|&i| self.items[i].pin_slot()).collect();
        let flags: Vec<bool> = slots.iter().map(|&i| self.items[i].kind == Kind::Divider).collect();
        let mut gone = Vec::new();
        for &k in crate::config::redundant_dividers(&flags).iter().rev() {
            let it = self.items.remove(slots[k]);
            gone.push(it.path.clone());
            self.poof(it);
        }
        if !gone.is_empty() {
            crate::log!("dock: {} redundant divider(s) left out", gone.len());
        }
        gone
    }
}

/// Pure: the dividers that went along with a removal (`dropped`, left
/// redundant by it), each with its place in the pinned order before it
/// (`before`, paths), for the undo.
pub(super) fn went_along(before: &[String], dropped: &[String]) -> Vec<(PinEntry, usize)> {
    dropped.iter().map(|g| (PinEntry::new(String::new(), g.clone()), before.iter().position(|p| p == g).unwrap_or(before.len()))).collect()
}

/// Pure: the box a divider is in (canonical DIP): its slot along the dock
/// (`dx`, `w`) and, across, the line's own extent (line_span) as drawn
/// at rest on `baseline` for icons of `icon`. (An icon's box is its
/// square; a divider's line is shorter and centered on the icons.)
pub(super) fn divider_box(dx: f32, w: f32, baseline: f32, icon: f32) -> Rect {
    let (top, h) = line_span(baseline, icon);
    Rect { x: dx, y: top, w, h }
}

/// Pure: where a separator's or divider's line is across the dock at rest
/// (top, length): half an icon long, centered on the icons.
pub(super) fn line_span(baseline: f32, icon: f32) -> (f32, f32) {
    let h = icon * 0.5;
    (baseline - icon / 2.0 - h / 2.0, h)
}

/// Pure: whether the system separator shows, for the items (kind,
/// pinned, present) in dock order: something follows the pins (a running
/// app not pinned, the Recycle Bin), and no divider ends the pins (then
/// that divider is the one line between them, in its own slot; the
/// separator gives up its slot). The pins on disk stay as they are: with
/// nothing after them the divider shows at their end again.
pub(super) fn system_sep_shown(items: &[(Kind, bool, bool)]) -> bool {
    let after = items.iter().any(|&(k, pinned, present)| (k == Kind::App && !pinned && present) || k == Kind::Trash);
    let ends_with_divider = items.iter().rfind(|&&(k, pinned, _)| pinned && k.movable()).is_some_and(|&(k, ..)| k == Kind::Divider);
    after && !ends_with_divider
}

/// Pure: whether Add Divider puts one after `pins` (the dock's pinned
/// entries): an app pinned, and no divider at the end already.
pub(super) fn can_add(pins: &[PinEntry]) -> bool {
    pins.iter().any(|p| !p.is_divider()) && !pins.last().is_some_and(PinEntry::is_divider)
}

/// Pure: the newest pins on disk with divider `entry` added at their end
/// (once), the redundant ones left out.
pub(super) fn with_divider(latest: &[PinEntry], entry: &PinEntry) -> Vec<PinEntry> {
    let mut out = latest.to_vec();
    if !out.iter().any(|p| p.path.eq_ignore_ascii_case(&entry.path)) {
        out.push(entry.clone());
    }
    normalize_dividers(&out)
}

/// Pure: `pins` without the entries at `gone` (paths, any case), and
/// without the dividers that leaves redundant.
pub(super) fn without(pins: &[PinEntry], gone: &[String]) -> Vec<PinEntry> {
    let left: Vec<PinEntry> = pins.iter().filter(|p| !gone.iter().any(|g| g.eq_ignore_ascii_case(&p.path))).cloned().collect();
    normalize_dividers(&left)
}

/// Pure: a drag's new order (`ours`, `dragged` moved) merged into the
/// newest pins on disk (drag::merge_pins), minus the dividers the move
/// left redundant (`gone`).
pub(super) fn reordered(latest: &[PinEntry], ours: &[PinEntry], dragged: &str, gone: &[String]) -> Vec<PinEntry> {
    without(&drag::merge_pins(latest, ours, dragged), gone)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PinEntry {
        PinEntry::new(s.into(), s.into())
    }

    fn d(n: u32) -> PinEntry {
        PinEntry::new(String::new(), format!("nopbar:divider:{n}"))
    }

    fn paths(v: &[PinEntry]) -> Vec<String> {
        v.iter().map(|e| if e.is_divider() { "|".to_string() + &e.path[15..] } else { e.path.clone() }).collect()
    }

    #[test]
    fn add_goes_after_the_pins_once() {
        let pins = vec![p("A"), p("B")];
        let new = PinEntry::divider(&pins);
        assert!(can_add(&pins));
        let out = with_divider(&pins, &new);
        assert_eq!(out.len(), 3);
        assert_eq!(out[2], new, "at the end, and it stays there");
        // Saved twice (the dock, then the file): still once.
        assert_eq!(with_divider(&out, &new), out);
        // One at the end already: Add makes no twin.
        assert!(!can_add(&out));
        // A twin at the end on disk: one stays.
        let disk = vec![p("A"), d(1)];
        assert_eq!(paths(&with_divider(&disk, &d(2))), ["A", "|1"]);
        // Nothing pinned: nothing to set apart.
        assert!(!can_add(&[]));
        assert!(with_divider(&[], &d(3)).is_empty());
    }

    #[test]
    fn remove_takes_it_and_what_it_leaves_redundant() {
        let pins = vec![p("A"), d(1), p("B"), d(2), p("C")];
        assert_eq!(paths(&without(&pins, &[d(1).path])), ["A", "B", "|2", "C"]);
        // Case does not matter.
        assert_eq!(paths(&without(&pins, &["NOPBAR:DIVIDER:2".into()])), ["A", "|1", "B", "C"]);
        // An app off the dock: a divider it leaves at the end stays ...
        assert_eq!(paths(&without(&pins, &["C".into()])), ["A", "|1", "B", "|2"]);
        // ... one it leaves at the start goes ...
        assert_eq!(paths(&without(&pins, &["A".into()])), ["B", "|2", "C"]);
        // ... and two left side by side become one.
        assert_eq!(paths(&without(&pins, &["B".into()])), ["A", "|1", "C"]);
    }

    #[test]
    fn a_dragged_divider_keeps_its_identity() {
        // Dividers are told apart by path: two of them never merge into one.
        let latest = vec![p("A"), d(1), p("B"), d(2), p("C"), p("D")];
        // Divider 2 dragged between C and D.
        let ours = vec![p("A"), d(1), p("B"), p("C"), d(2), p("D")];
        assert_eq!(paths(&reordered(&latest, &ours, &d(2).path, &[])), ["A", "|1", "B", "C", "|2", "D"]);
        // Dragged to the very end: it stays there.
        let ours = vec![p("A"), d(1), p("B"), p("C"), p("D"), d(2)];
        assert_eq!(paths(&reordered(&latest, &ours, &d(2).path, &[])), ["A", "|1", "B", "C", "D", "|2"]);
        // Dragged to the start: the dock dropped it and says so in `gone`.
        let ours = vec![d(2), p("A"), d(1), p("B"), p("C"), p("D")];
        assert_eq!(paths(&reordered(&latest, &ours, &d(2).path, &[d(2).path])), ["A", "|1", "B", "C", "D"]);
        // Pinned elsewhere meanwhile (on disk only): kept, at the end.
        let latest2 = [latest.clone(), vec![p("X")]].concat();
        let ours = vec![p("A"), p("B"), d(1), d(2), p("C"), p("D")];
        // (Side by side after the move: the dock dropped divider 2.)
        assert_eq!(paths(&reordered(&latest2, &ours, &d(1).path, &[d(2).path])), ["A", "B", "|1", "C", "D", "X"]);
    }

    #[test]
    fn dividers_move_and_are_removed_like_pins() {
        // Only apps and dividers are dragged; only lines skip magnification.
        assert!(Kind::Divider.movable() && Kind::App.movable());
        assert!(!Kind::Sep.movable() && !Kind::Start.movable() && !Kind::Trash.movable());
        assert!(Kind::Divider.is_line() && Kind::Sep.is_line() && !Kind::App.is_line());
        // A pinned divider pulled off the dock is in the remove zone, like
        // a pinned app (drag::zone does not look at the kind).
        let bar = Rect { x: 100.0, y: 200.0, w: 300.0, h: 68.0 };
        assert_eq!(drag::zone(150.0, bar.y - drag::REMOVE_DIST - 1.0, bar, 276.0), drag::Zone::Remove);
        // Ctrl+Z puts a removed divider back where it was.
        let pins = vec![p("A"), p("B")];
        assert_eq!(paths(&drag::reinsert_pin(&pins, &d(1), 1).unwrap()), ["A", "|1", "B"]);
    }

    #[test]
    fn a_divider_takes_the_separators_slot() {
        let icon = 52.0;
        assert_eq!(Kind::Divider.base_w(icon), SEP_W);
        assert_eq!(Kind::Divider.base_w(icon), Kind::Sep.base_w(icon));
        // Never magnified: the pointer on it changes nothing.
        assert_eq!(Kind::Divider.drawn_w(icon, 2.0, 1.0), SEP_W);
        assert_eq!(Kind::Divider.drawn_w(icon, 2.0, 0.5), SEP_W * 0.5, "grows in and out with presence");
        assert_eq!(Kind::App.drawn_w(icon, 2.0, 1.0), icon * 2.0);
        // A dock of A | B is two icons and a separator's slot wide.
        let row = [Kind::App, Kind::Divider, Kind::App];
        let total: f32 = row.iter().map(|k| k.drawn_w(icon, 1.0, 1.0)).sum();
        assert_eq!(total, icon * 2.0 + SEP_W);
    }

    #[test]
    fn a_divider_at_the_end_and_the_separator_are_one_line() {
        use Kind::*;
        let pinned = |k| (k, true, true);
        let running = (App, false, true);
        let gone = (App, false, false);
        let trash = (Trash, true, true);
        let sep = (Sep, true, false);
        // A B | running: the separator shows.
        assert!(system_sep_shown(&[pinned(App), pinned(App), sep, running]));
        // A B (divider) | running: the divider stands for it, one slot.
        assert!(!system_sep_shown(&[pinned(App), pinned(App), pinned(Divider), sep, running]));
        assert!(!system_sep_shown(&[pinned(App), pinned(Divider), sep, trash]));
        // Nothing after the pins: no separator; the divider shows at the end.
        assert!(!system_sep_shown(&[pinned(App), pinned(Divider), sep, gone]));
        assert!(!system_sep_shown(&[pinned(App), sep]));
        // A divider between apps (not at the end): the separator shows too.
        assert!(system_sep_shown(&[pinned(App), pinned(Divider), pinned(App), sep, running]));
        assert!(system_sep_shown(&[(Start, true, true), pinned(App), sep, trash]));
        // One line, one slot: a divider and a hidden separator are as
        // wide as the separator alone.
        let icon = 52.0;
        let merged = Divider.drawn_w(icon, 1.0, 1.0) + Sep.drawn_w(icon, 1.0, 0.0);
        assert_eq!(merged, Sep.drawn_w(icon, 1.0, 1.0));
        // The keyboard still goes past it, and it stays a divider for the
        // pointer (menu, drag): the separator never was.
        let items = [(App, true), (Divider, true), (Sep, false), (App, true)];
        assert_eq!(super::access::keyboard_stops(&items), [0, 3]);
    }

    #[test]
    fn a_divider_removed_from_its_menu_comes_back_with_ctrl_z() {
        // A | B: "Remove Divider", then Ctrl+Z.
        let before: Vec<String> = vec!["A".into(), d(1).path, "B".into()];
        let index = before.iter().position(|p| *p == d(1).path).unwrap();
        let now = without(&[p("A"), d(1), p("B")], &[d(1).path]);
        assert_eq!(paths(&now), ["A", "B"]);
        let back = drag::reinsert_pins(&now, &PinEntry::new(String::new(), d(1).path), index, &went_along(&before, &[])).unwrap();
        assert_eq!(paths(&back), ["A", "|1", "B"]);
        // A divider that went along (here: left at the start) comes back too.
        let before: Vec<String> = vec![d(2).path, "A".into(), d(1).path, "B".into()];
        let also = went_along(&before, &[d(2).path]);
        assert_eq!(also, vec![(d(2), 0)]);
    }

    #[test]
    fn a_dividers_screen_reader_box_is_on_its_line() {
        let (icon, baseline) = (52.0, 300.0);
        let b = divider_box(100.0, SEP_W, baseline, icon);
        let (top, h) = line_span(baseline, icon);
        // Across: exactly the line drawn at rest (baseline-39 .. baseline-13).
        assert_eq!((b.y, b.y + b.h), (top, top + h));
        assert_eq!((top, top + h), (baseline - 39.0, baseline - 13.0));
        // Along: the slot the pointer hits.
        assert_eq!((b.x, b.w), (100.0, SEP_W));
        // (An icon's box, its width tall on the baseline, would miss it.)
        let icon_like = (baseline - SEP_W, baseline);
        assert!(icon_like.0 > top + h);
    }

    #[test]
    fn the_keyboard_goes_past_dividers() {
        use super::access::{a11y_listed, key_move, keyboard_stops, KeyMove};
        // Start, A, divider, B, separator (never clickable), trash.
        let items = [(Kind::Start, true), (Kind::App, true), (Kind::Divider, true), (Kind::App, true), (Kind::Sep, false), (Kind::Trash, true)];
        let stops = keyboard_stops(&items);
        assert_eq!(stops, [0, 1, 3, 5]);
        // Right from A lands on B.
        let at_a = stops.iter().position(|&i| i == 1).unwrap();
        assert_eq!(key_move(windows::Win32::UI::Input::KeyboardAndMouse::VK_RIGHT.0, false, at_a, stops.len()), KeyMove::To(at_a + 1));
        assert_eq!(stops[at_a + 1], 3);
        // UIA lists the divider (as a separator) between them.
        assert_eq!(a11y_listed(&items), [0, 1, 2, 3, 5]);
    }
}
