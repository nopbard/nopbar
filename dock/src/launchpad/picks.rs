//! Several icons at once in edit mode (pure): which are picked (Ctrl+click
//! toggles one, Shift+click picks a run from the last one clicked, a band
//! dragged over empty space picks what it touches), taking them out of the
//! pages together and putting them down together (their order kept, at
//! the drop point or into a folder), and "Group into Folder".
//!
//! Picked items are known by who they are (an app by its id, a folder by
//! its session id), not where they are: the pages change under them.

use std::collections::HashSet;

use super::edit::{self, Pos};
use super::grid::Rect;
use super::layout_store::Pages;
use crate::config::LpItem;

/// Who an item on the pages is.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    App(String),
    Folder(u64),
}

pub fn key_of(it: &LpItem) -> Key {
    match it {
        LpItem::App(id) => Key::App(id.clone()),
        LpItem::Folder { id, .. } => Key::Folder(*id),
    }
}

/// Where `k` is on the pages.
pub fn find(pages: &Pages, k: &Key) -> Option<Pos> {
    pages.iter().enumerate().find_map(|(p, v)| v.iter().position(|it| key_of(it) == *k).map(|s| Pos::new(p, s)))
}

/// The picked items, and the one a Shift+click runs from.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Picks {
    keys: Vec<Key>,
    anchor: Option<Key>,
}

impl Picks {
    pub fn len(&self) -> usize {
        self.keys.len()
    }
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
    pub fn contains(&self, k: &Key) -> bool {
        self.keys.contains(k)
    }
    pub fn keys(&self) -> &[Key] {
        &self.keys
    }
    pub fn clear(&mut self) {
        self.keys.clear();
        self.anchor = None;
    }

    /// A plain click on `k`: nothing picked, but a Shift+click runs from it.
    pub fn set_anchor(&mut self, k: Key) {
        self.anchor = Some(k);
    }

    /// Ctrl+click: `k` in or out. True: it is picked now.
    pub fn toggle(&mut self, k: Key) -> bool {
        self.anchor = Some(k.clone());
        if let Some(i) = self.keys.iter().position(|x| *x == k) {
            self.keys.remove(i);
            false
        } else {
            self.keys.push(k);
            true
        }
    }

    /// Shift+click on `to`: everything from the anchor (the last one
    /// clicked; none: `to` alone) to `to` in the pages' order, across
    /// pages. `add` (Ctrl held too): added to what is picked, else instead
    /// of it. The anchor stays.
    pub fn range(&mut self, pages: &Pages, to: Key, add: bool) {
        let flat: Vec<Key> = pages.iter().flatten().map(key_of).collect();
        let Some(b) = flat.iter().position(|k| *k == to) else { return };
        let a = self.anchor.as_ref().and_then(|x| flat.iter().position(|k| k == x)).unwrap_or(b);
        let (lo, hi) = (a.min(b), a.max(b));
        if !add {
            self.keys.clear();
        }
        for k in &flat[lo..=hi] {
            if !self.keys.contains(k) {
                self.keys.push(k.clone());
            }
        }
        if self.anchor.is_none() {
            self.anchor = Some(to);
        }
    }

    /// A band: these are picked (`add`: on top of `base`, what was picked
    /// when the band started).
    pub fn set(&mut self, base: &[Key], band: Vec<Key>, add: bool) {
        self.keys = if add { base.to_vec() } else { Vec::new() };
        for k in band {
            if !self.keys.contains(&k) {
                self.keys.push(k);
            }
        }
    }

    /// Only what is still on the pages stays picked.
    pub fn prune(&mut self, pages: &Pages) {
        let on: HashSet<Key> = pages.iter().flatten().map(key_of).collect();
        self.keys.retain(|k| on.contains(k));
        if self.anchor.as_ref().is_some_and(|a| !on.contains(a)) {
            self.anchor = None;
        }
    }

    /// Where the picked items are, in the pages' order.
    pub fn positions(&self, pages: &Pages) -> Vec<Pos> {
        let mut v: Vec<Pos> = self.keys.iter().filter_map(|k| find(pages, k)).collect();
        v.sort_by_key(|p| (p.page, p.slot));
        v
    }
}

/// What a band `band` touches: the items whose box (`boxes`: each item's
/// box on screen and who it is) it overlaps.
pub fn in_band(band: &Rect, boxes: &[(Rect, Key)]) -> Vec<Key> {
    boxes.iter().filter(|(r, _)| band.overlaps(r)).map(|(_, k)| k.clone()).collect()
}

/// Takes the items at `at` out (in the pages' order; pages stay, maybe
/// empty).
pub fn take_many(pages: &mut Pages, at: &[Pos]) -> Vec<LpItem> {
    let mut ps: Vec<Pos> = at.to_vec();
    ps.sort_by_key(|p| (p.page, p.slot));
    ps.dedup();
    let mut out: Vec<LpItem> = ps.iter().rev().filter_map(|p| edit::take(pages, *p)).collect();
    out.reverse();
    out
}

/// Puts `items` in at `at` in their order (like edit::put: no holes, a
/// page past the last a new one, what a full page cannot hold pushed on to
/// the next). Where the first landed.
pub fn put_many(pages: &mut Pages, at: Pos, items: Vec<LpItem>, per_page: usize) -> Pos {
    let pp = per_page.max(1);
    let page = at.page.min(pages.len());
    if page == pages.len() {
        pages.push(Vec::new());
    }
    let slot = at.slot.min(pages[page].len());
    pages[page].splice(slot..slot, items);
    let mut p = page;
    while pages[p].len() > pp {
        let over = pages[p].split_off(pp);
        if p + 1 == pages.len() {
            pages.push(Vec::new());
        }
        pages[p + 1].splice(0..0, over);
        p += 1;
    }
    // (A slot past the page's room went on to the next page.)
    if slot >= pp { Pos::new(page + 1, slot - pp) } else { Pos::new(page, slot) }
}

/// The keyboard (or a screen reader's Move earlier / later) moves the
/// picked items at `at` one place with arrow `key`, `lead` (one of them)
/// the one with the focus: they come out together and go back in together
/// (their order kept, like a drag: put_many) one place on from where the
/// lead was among the rest, left / right through the pages (a new last
/// page past the end), up / down a row on its page. Empty pages go. Where
/// the lead is after, None if they cannot go that way (the pages as they
/// were).
pub fn move_many(pages: &mut Pages, at: &[Pos], lead: Pos, g: &super::grid::Grid, key: super::grid::Arrow, rtl: bool, per_page: usize) -> Option<Pos> {
    use super::grid::Arrow;
    let mut ps: Vec<Pos> = at.to_vec();
    ps.sort_by_key(|p| (p.page, p.slot));
    ps.dedup();
    let lead_key = key_of(pages.get(lead.page)?.get(lead.slot)?);
    if !ps.contains(&lead) || ps.iter().any(|p| pages.get(p.page).and_then(|v| v.get(p.slot)).is_none()) {
        return None;
    }
    let before = pages.clone();
    // Where the lead was once they are out.
    let gap = lead.slot - ps.iter().filter(|p| p.page == lead.page && p.slot < lead.slot).count();
    let items = take_many(pages, &ps);
    let page = lead.page;
    let len = pages[page].len();
    let cols = g.cols.max(1);
    let to = match key {
        Arrow::Up | Arrow::Down => {
            let row = gap / cols;
            let to_row = if key == Arrow::Down { Some(row + 1) } else { row.checked_sub(1) };
            to_row.filter(|&r| r < g.rows && r * cols <= len).map(|r| Pos::new(page, (r * cols + gap % cols).min(len)))
        }
        _ => {
            let forward = (key == Arrow::Right) != rtl;
            if forward {
                if gap < len {
                    Some(Pos::new(page, gap + 1))
                } else if page + 1 < pages.len() {
                    Some(Pos::new(page + 1, 0))
                } else if len > 0 {
                    // Past the end, with others on its page: a new last page.
                    Some(Pos::new(pages.len(), 0))
                } else {
                    None
                }
            } else if gap > 0 {
                Some(Pos::new(page, gap - 1))
            } else if page > 0 {
                Some(Pos::new(page - 1, pages[page - 1].len()))
            } else {
                None
            }
        }
    };
    let Some(to) = to else {
        *pages = before;
        return None;
    };
    put_many(pages, to, items, per_page);
    edit::tidy(pages, None);
    find(pages, &lead_key)
}

/// Drops `items` onto the item at `onto`: onto an app, a folder named
/// `name` of it and them; onto a folder, into it at its end. Only apps go
/// in folders (no folders in folders): with a folder among them, or
/// nothing at `onto`, Err(items) back.
pub fn combine_many(pages: &mut Pages, onto: Pos, items: Vec<LpItem>, name: &str) -> Result<(), Vec<LpItem>> {
    if items.is_empty() || items.iter().any(|it| matches!(it, LpItem::Folder { .. })) {
        return Err(items);
    }
    let Some(target) = pages.get_mut(onto.page).and_then(|p| p.get_mut(onto.slot)) else { return Err(items) };
    let ids: Vec<String> = items.iter().filter_map(|it| if let LpItem::App(id) = it { Some(id.clone()) } else { None }).collect();
    match target {
        LpItem::App(t) => {
            let mut apps = vec![t.clone()];
            apps.extend(ids.into_iter().filter(|id| id != t));
            if apps.len() < 2 {
                return Err(items);
            }
            *target = LpItem::Folder { name: super::folders::clean_name(name), apps, id: super::folders::new_id(), auto: false };
        }
        LpItem::Folder { apps, auto, .. } => {
            for id in ids {
                if !apps.contains(&id) {
                    apps.push(id);
                    *auto = false;
                }
            }
        }
    }
    Ok(())
}

/// "Group into Folder" for the items at `at` (two or more): into the first
/// folder among them (its name kept; the other folders' apps go in with
/// the rest), else into a new folder named `name`, where the first of them
/// was. Empty pages go. Where the folder is, and whether it is new.
pub fn group(pages: &mut Pages, at: &[Pos], name: &str, per_page: usize) -> Option<(Pos, bool)> {
    let mut ps: Vec<Pos> = at.to_vec();
    ps.sort_by_key(|p| (p.page, p.slot));
    ps.dedup();
    if ps.len() < 2 || ps.iter().any(|p| pages.get(p.page).and_then(|v| v.get(p.slot)).is_none()) {
        return None;
    }
    let first = ps[0];
    let items = take_many(pages, &ps);
    let mut ids: Vec<String> = Vec::new();
    let mut add = |id: &String| {
        if !ids.contains(id) {
            ids.push(id.clone());
        }
    };
    let target = items.iter().position(|it| matches!(it, LpItem::Folder { .. }));
    if let Some(LpItem::Folder { apps, .. }) = target.map(|t| &items[t]) {
        apps.iter().for_each(&mut add);
    }
    for it in &items {
        match it {
            LpItem::App(id) => add(id),
            LpItem::Folder { apps, .. } => apps.iter().for_each(&mut add),
        }
    }
    let folder = match target.map(|t| &items[t]) {
        // (Others put in by the user: no longer Tidy Home's.)
        Some(LpItem::Folder { name, id, .. }) => LpItem::Folder { name: name.clone(), apps: ids, id: *id, auto: false },
        _ => LpItem::Folder { name: super::folders::clean_name(name), apps: ids, id: super::folders::new_id(), auto: false },
    };
    let landed = put_many(pages, first, vec![folder], per_page);
    let landed = edit::tidy(pages, Some(landed)).unwrap_or(Pos::new(0, 0));
    Some((landed, target.is_none()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str) -> LpItem {
        LpItem::App(id.into())
    }
    fn page(ids: &[&str]) -> Vec<LpItem> {
        ids.iter().map(|i| app(i)).collect()
    }
    fn k(id: &str) -> Key {
        Key::App(id.into())
    }
    fn folder(name: &str, apps: &[&str], id: u64) -> LpItem {
        LpItem::Folder { name: name.into(), apps: apps.iter().map(|s| s.to_string()).collect(), id, auto: false }
    }

    #[test]
    fn ctrl_click_toggles() {
        let mut p = Picks::default();
        assert!(p.toggle(k("a")) && p.toggle(k("c")));
        assert_eq!(p.keys(), [k("a"), k("c")]);
        assert!(!p.toggle(k("a")), "again: out");
        assert_eq!(p.keys(), [k("c")]);
        p.clear();
        assert!(p.is_empty());
    }

    #[test]
    fn shift_click_picks_a_run_across_pages() {
        let pages = vec![page(&["a", "b", "c"]), page(&["d", "e"])];
        let mut p = Picks::default();
        p.toggle(k("b"));
        p.range(&pages, k("d"), false);
        assert_eq!(p.keys(), [k("b"), k("c"), k("d")]);
        // Backwards from the same anchor, instead of the run before.
        p.range(&pages, k("a"), false);
        assert_eq!(p.keys(), [k("a"), k("b")]);
        // Ctrl+Shift adds.
        p.toggle(k("e"));
        p.range(&pages, k("d"), true);
        assert!(p.contains(&k("a")) && p.contains(&k("d")) && p.contains(&k("e")));
        // A plain click sets where the next Shift+click runs from.
        let mut a = Picks::default();
        a.set_anchor(k("e"));
        assert!(a.is_empty());
        a.range(&pages, k("c"), false);
        assert_eq!(a.keys(), [k("c"), k("d"), k("e")]);
        // No anchor yet: just that one.
        let mut q = Picks::default();
        q.range(&pages, k("c"), false);
        assert_eq!(q.keys(), [k("c")]);
        q.range(&pages, k("zz"), false);
        assert_eq!(q.keys(), [k("c")], "not on the pages: nothing changes");
    }

    #[test]
    fn a_band_picks_what_it_touches() {
        let boxes = vec![(Rect::new(0.0, 0.0, 10.0, 10.0), k("a")), (Rect::new(20.0, 0.0, 30.0, 10.0), k("b")), (Rect::new(0.0, 20.0, 10.0, 30.0), k("c"))];
        assert_eq!(in_band(&Rect::new(5.0, 5.0, 25.0, 8.0), &boxes), [k("a"), k("b")]);
        assert!(in_band(&Rect::new(12.0, 12.0, 18.0, 18.0), &boxes).is_empty());
        let mut p = Picks::default();
        p.toggle(k("z"));
        let base = p.keys().to_vec();
        p.set(&base, vec![k("a")], false);
        assert_eq!(p.keys(), [k("a")], "a band alone: instead");
        p.set(&base, vec![k("a"), k("z")], true);
        assert_eq!(p.keys(), [k("z"), k("a")], "with Ctrl: on top, nothing twice");
    }

    #[test]
    fn picks_follow_items_not_places() {
        let mut pages = vec![page(&["a", "b", "c"]), vec![folder("F", &["x", "y"], 7)]];
        let mut p = Picks::default();
        p.toggle(Key::Folder(7));
        p.toggle(k("a"));
        assert_eq!(p.positions(&pages), [Pos::new(0, 0), Pos::new(1, 0)], "in the pages' order");
        pages[0].remove(0);
        pages[0].insert(1, app("a"));
        assert_eq!(p.positions(&pages), [Pos::new(0, 1), Pos::new(1, 0)]);
        pages[0].retain(|it| *it != app("a"));
        p.prune(&pages);
        assert_eq!(p.keys(), [Key::Folder(7)]);
    }

    #[test]
    fn many_move_together_in_their_order() {
        let mut pages = vec![page(&["a", "b", "c", "d"]), page(&["e", "f"])];
        let taken = take_many(&mut pages, &[Pos::new(1, 0), Pos::new(0, 1), Pos::new(0, 3)]);
        assert_eq!(taken, page(&["b", "d", "e"]), "in the pages' order, whatever order asked");
        assert_eq!(pages, vec![page(&["a", "c"]), page(&["f"])]);
        let at = put_many(&mut pages, Pos::new(1, 1), taken, 4);
        assert_eq!(at, Pos::new(1, 1));
        assert_eq!(pages, vec![page(&["a", "c"]), page(&["f", "b", "d", "e"])]);
        // Into a fuller page: the overflow goes on.
        let mut pages = vec![page(&["a", "b", "c"]), page(&["d"])];
        let at = put_many(&mut pages, Pos::new(0, 1), page(&["x", "y"]), 3);
        assert_eq!(at, Pos::new(0, 1));
        assert_eq!(pages, vec![page(&["a", "x", "y"]), page(&["b", "c", "d"])]);
        // Past the last page: a new one.
        let mut pages = vec![page(&["a"])];
        assert_eq!(put_many(&mut pages, Pos::new(5, 0), page(&["x", "y"]), 3), Pos::new(1, 0));
        assert_eq!(pages, vec![page(&["a"]), page(&["x", "y"])]);
    }

    #[test]
    fn many_dropped_onto_an_icon_make_a_folder() {
        let mut pages = vec![page(&["a", "b"])];
        assert_eq!(combine_many(&mut pages, Pos::new(0, 0), page(&["x", "y"]), "New"), Ok(()));
        assert_eq!(pages, vec![vec![LpItem::folder("New", vec!["a".into(), "x".into(), "y".into()]), app("b")]]);
        // Onto that folder: in at its end, nothing twice.
        assert_eq!(combine_many(&mut pages, Pos::new(0, 0), page(&["z", "x"]), "New"), Ok(()));
        assert_eq!(pages[0][0], LpItem::folder("New", vec!["a".into(), "x".into(), "y".into(), "z".into()]));
        // A folder among them, or nothing there: no.
        let with_folder = vec![app("q"), folder("G", &["m", "n"], 3)];
        assert_eq!(combine_many(&mut pages, Pos::new(0, 1), with_folder.clone(), "N"), Err(with_folder));
        assert_eq!(combine_many(&mut pages, Pos::new(0, 9), page(&["q"]), "N"), Err(page(&["q"])));
        assert_eq!(combine_many(&mut pages, Pos::new(0, 1), page(&["b"]), "N"), Err(page(&["b"])), "onto itself alone");
    }

    fn grid(cols: usize, rows: usize) -> super::super::grid::Grid {
        super::super::grid::Grid { cols, rows, left: 0.0, top: 0.0, cell_w: 100.0, cell_h: 100.0, label_h: super::super::grid::LABEL_H, icon_edge: super::super::grid::ICON }
    }

    /// The review's case: the keyboard (and a screen reader's Move) moves
    /// every picked item, apps and folders on different pages, as a drag
    /// to the same place does.
    #[test]
    fn the_keyboard_moves_every_picked_item() {
        use super::super::grid::Arrow;
        let g = grid(2, 2);
        let start = vec![page(&["a", "b", "c", "d"]), vec![app("e"), folder("F", &["x", "y"], 5), app("g")]];
        let picked = [Pos::new(0, 1), Pos::new(1, 1)];
        // Lead b: right one place (after c among the rest).
        let mut p = start.clone();
        let to = move_many(&mut p, &picked, Pos::new(0, 1), &g, Arrow::Right, false, 4);
        assert_eq!(p, vec![vec![app("a"), app("c"), app("b"), folder("F", &["x", "y"], 5)], page(&["d", "e", "g"])]);
        assert_eq!(to, Some(Pos::new(0, 2)), "the lead where it went");
        // The same as a drag dropped there: taken out together, put in together.
        let mut q = start.clone();
        let items = take_many(&mut q, &picked);
        put_many(&mut q, Pos::new(0, 2), items, 4);
        edit::tidy(&mut q, None);
        assert_eq!(p, q);
        // Left from the page's start: the end of the page before (overflow on).
        let mut p = start.clone();
        let to = move_many(&mut p, &[Pos::new(1, 0), Pos::new(1, 2)], Pos::new(1, 0), &g, Arrow::Left, false, 4);
        assert_eq!(p, vec![page(&["a", "b", "c", "d"]), vec![app("e"), app("g"), folder("F", &["x", "y"], 5)]]);
        assert_eq!(to, Some(Pos::new(1, 0)), "a full page before: they are pushed back on");
        // Down a row; mirrored right to left.
        let mut p = vec![page(&["a", "b", "c", "d"])];
        assert_eq!(move_many(&mut p, &[Pos::new(0, 0), Pos::new(0, 1)], Pos::new(0, 0), &g, Arrow::Down, false, 4), Some(Pos::new(0, 2)));
        assert_eq!(p, vec![page(&["c", "d", "a", "b"])]);
        let mut p = vec![page(&["a", "b", "c"])];
        assert_eq!(move_many(&mut p, &[Pos::new(0, 0)], Pos::new(0, 0), &g, Arrow::Left, true, 4), Some(Pos::new(0, 1)), "right to left: Left goes on");
        // Nowhere to go: unchanged.
        let mut p = vec![page(&["a", "b", "c"])];
        assert_eq!(move_many(&mut p, &[Pos::new(0, 0), Pos::new(0, 1)], Pos::new(0, 0), &g, Arrow::Left, false, 4), None);
        assert_eq!(move_many(&mut p, &[Pos::new(0, 0), Pos::new(0, 1)], Pos::new(0, 0), &g, Arrow::Up, false, 4), None);
        assert_eq!(move_many(&mut p, &[Pos::new(0, 0)], Pos::new(0, 2), &g, Arrow::Right, false, 4), None, "the lead must be picked");
        assert_eq!(p, vec![page(&["a", "b", "c"])]);
        // Past the end with others on the page: a new last page.
        let mut p = vec![page(&["a", "b", "c"])];
        assert_eq!(move_many(&mut p, &[Pos::new(0, 1), Pos::new(0, 2)], Pos::new(0, 2), &g, Arrow::Right, false, 4), Some(Pos::new(1, 1)));
        assert_eq!(p, vec![page(&["a"]), page(&["b", "c"])]);
    }

    #[test]
    fn group_into_a_folder() {
        // Apps only: a new folder where the first was.
        let mut pages = vec![page(&["a", "b", "c"]), page(&["d"])];
        assert_eq!(group(&mut pages, &[Pos::new(1, 0), Pos::new(0, 1)], "New", 4), Some((Pos::new(0, 1), true)));
        assert_eq!(pages, vec![vec![app("a"), LpItem::folder("New", vec!["b".into(), "d".into()]), app("c")]], "the emptied page went");
        // With a folder: into the first folder, its name and id kept.
        let mut pages = vec![vec![app("a"), folder("Mine", &["m"], 9), folder("Other", &["o1", "o2"], 4), app("b")]];
        let got = group(&mut pages, &[Pos::new(0, 0), Pos::new(0, 1), Pos::new(0, 2)], "New", 9);
        assert_eq!(got, Some((Pos::new(0, 0), false)));
        assert_eq!(pages, vec![vec![folder("Mine", &["m", "a", "o1", "o2"], 9), app("b")]]);
        assert!(matches!(pages[0][0], LpItem::Folder { id: 9, .. }));
        // One alone, or a place with nothing: no.
        let mut pages = vec![page(&["a", "b"])];
        assert_eq!(group(&mut pages, &[Pos::new(0, 0)], "N", 4), None);
        assert_eq!(group(&mut pages, &[Pos::new(0, 0), Pos::new(0, 5)], "N", 4), None);
        assert_eq!(pages, vec![page(&["a", "b"])], "untouched");
    }
}
