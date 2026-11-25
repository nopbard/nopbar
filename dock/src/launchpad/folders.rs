//! Launchpad folders (pure): made by dropping one app on another, more
//! apps dropped in, apps dragged out (a folder left with one app is that
//! app again), the mini grid on a folder's tile and the open folder's panel.

use super::edit::Pos;
use super::grid::{Grid, Rect};
use super::layout_store::Pages;
use crate::config::LpItem;

/// A folder's tile shows its first apps in a grid this many a side.
pub const MINI_SIDE: usize = 3;
pub const MINI: usize = MINI_SIDE * MINI_SIDE;
/// The open folder's panel: at most this many columns...
pub const PANEL_COLS: usize = 5;
/// The panel is a big plate (design system 1.2 and 3.3: r.sheet): its
/// edge to what it holds (the apps' cells, the name's row) is PANEL_PAD,
/// its text starts a card's padding further in (PANEL_INK).
pub const PANEL_PAD: f32 = crate::theme::metrics::space::S5;
pub const PANEL_INK: f32 = PANEL_PAD + crate::theme::metrics::size::CARD_PAD;
/// The name's row (a field while the name is changed) and, with the
/// padding above it and the gap under it, the band above the apps.
pub const NAME_H: f32 = crate::theme::metrics::size::FIELD_L;
pub const PANEL_TITLE: f32 = PANEL_PAD + NAME_H + crate::theme::metrics::space::S3;
/// The folder's name: the ramp's title (20, semibold).
pub const TITLE: crate::theme::ramp::TextStyle = crate::theme::ramp::TITLE;
/// Names are kept to this many characters.
pub const MAX_NAME: usize = 40;

static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// A folder's session id (LpItem::Folder::id): never 0, never twice.
pub fn new_id() -> u64 {
    NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Where the folder with session id `id` is on the pages.
pub fn find(pages: &Pages, id: u64) -> Option<Pos> {
    pages.iter().enumerate().find_map(|(p, v)| v.iter().position(|it| matches!(it, LpItem::Folder { id: f, .. } if *f == id)).map(|s| Pos::new(p, s)))
}

/// Drops `item` onto the item at `onto`: two apps become a folder named
/// `name`; an app dropped on a folder goes in at its end. A folder dropped
/// on anything (no folders in folders), or onto nothing: Err(item) back
/// (the caller puts it down as a move).
pub fn combine(pages: &mut Pages, onto: Pos, item: LpItem, name: &str) -> Result<(), LpItem> {
    let LpItem::App(id) = item else { return Err(item) };
    let Some(target) = pages.get_mut(onto.page).and_then(|p| p.get_mut(onto.slot)) else {
        return Err(LpItem::App(id));
    };
    match target {
        LpItem::App(t) => {
            if *t == id {
                return Err(LpItem::App(id));
            }
            *target = LpItem::Folder { name: clean_name(name), apps: vec![t.clone(), id], id: new_id(), auto: false };
        }
        LpItem::Folder { apps, auto, .. } => {
            if !apps.contains(&id) {
                apps.push(id);
                // (Changed by the user: theirs now, not Tidy Home's.)
                *auto = false;
            }
        }
    }
    Ok(())
}

/// Takes app `idx` out of the folder at `at`. A folder left with one app is
/// that app again (in the folder's place). The app's id.
pub fn take_out(pages: &mut Pages, at: Pos, idx: usize) -> Option<String> {
    let item = pages.get_mut(at.page)?.get_mut(at.slot)?;
    let LpItem::Folder { apps, auto, .. } = item else { return None };
    if idx >= apps.len() {
        return None;
    }
    let id = apps.remove(idx);
    *auto = false;
    match apps.len() {
        0 => {
            pages[at.page].remove(at.slot);
        }
        1 => *item = LpItem::App(apps[0].clone()),
        _ => {}
    }
    Some(id)
}

/// Moves a folder's app `from` to `to` (inside the folder).
pub fn move_in(apps: &mut Vec<String>, from: usize, to: usize) {
    if from < apps.len() {
        let a = apps.remove(from);
        apps.insert(to.min(apps.len()), a);
    }
}

/// The apps shown on a folder's tile.
pub fn mini(apps: &[String]) -> &[String] {
    &apps[..apps.len().min(MINI)]
}

/// Where mini icon `k` goes on a folder tile `side` DIP wide (inside a
/// margin; row by row, mirrored right to left).
pub fn mini_rect(k: usize, side: f32, rtl: bool) -> Rect {
    let margin = side * 0.14;
    let gap = side * 0.05;
    let n = MINI_SIDE as f32;
    let cell = (side - margin * 2.0 - gap * (n - 1.0)) / n;
    let (row, col) = (k / MINI_SIDE, k % MINI_SIDE);
    let col = if rtl { MINI_SIDE - 1 - col } else { col };
    let l = margin + col as f32 * (cell + gap);
    let t = margin + row as f32 * (cell + gap);
    Rect::new(l, t, l + cell, t + cell)
}

/// What a folder is called on screen: its name, or the default (a new
/// folder's name in the UI language) when it has none.
pub fn shown_name(name: &str, default: &str) -> String {
    let n = name.trim();
    if n.is_empty() { default.to_string() } else { n.to_string() }
}

/// A name as kept: trimmed, one line, at most MAX_NAME characters.
pub fn clean_name(name: &str) -> String {
    let one: String = name.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    one.trim().chars().take(MAX_NAME).collect::<String>().trim_end().to_string()
}

/// The open folder's grid for `n` apps on the work area `w` x `h` DIP,
/// with cells like the main grid's (`g`): 3 to PANEL_COLS wide (never wider
/// than the main grid), as many rows as it needs, at most the main grid's
/// rows less one; centered below its name. Relative to the work area like
/// the main grid. Apps beyond its rows scroll (`rows_for`).
pub fn panel_grid(n: usize, g: &Grid, w: f32, h: f32) -> Grid {
    let cols = n.clamp(3, PANEL_COLS).min(g.cols.max(1));
    let max_rows = g.rows.saturating_sub(1).max(1);
    let rows = n.div_ceil(cols).clamp(1, max_rows);
    let cell_w = g.cell_w;
    let cell_h = g.cell_h.max(g.content_h());
    let gw = cell_w * cols as f32;
    let gh = cell_h * rows as f32;
    let left = (w - gw) / 2.0;
    // (The panel, its band above and its padding below, in the middle.)
    let top = ((h - gh - PANEL_TITLE - PANEL_PAD) / 2.0).max(0.0) + PANEL_TITLE;
    Grid { cols, rows, left, top, cell_w, cell_h, label_h: g.label_h, icon_edge: g.icon_edge }
}

/// The panel around a folder grid (relative to the work area).
pub fn panel_rect(pg: &Grid) -> Rect {
    Rect::new(pg.left - PANEL_PAD, pg.top - PANEL_TITLE, pg.left + pg.cell_w * pg.cols as f32 + PANEL_PAD, pg.top + pg.cell_h * pg.rows as f32 + PANEL_PAD)
}

/// The name's row in a panel `w` wide (from the panel's top left).
pub fn name_rect(w: f32) -> Rect {
    Rect::new(PANEL_PAD, PANEL_PAD, w - PANEL_PAD, PANEL_PAD + NAME_H)
}

/// The rows of a folder of `n` apps beyond what its panel shows: how far
/// it can scroll (in rows).
pub fn max_scroll(n: usize, pg: &Grid) -> usize {
    n.div_ceil(pg.cols.max(1)).saturating_sub(pg.rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str) -> LpItem {
        LpItem::App(id.into())
    }
    fn folder(name: &str, apps: &[&str]) -> LpItem {
        LpItem::folder(name, apps.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn two_apps_make_a_folder_and_more_go_in() {
        let mut p = vec![vec![app("a"), app("b")]];
        assert_eq!(combine(&mut p, Pos::new(0, 0), app("c"), "새 폴더"), Ok(()));
        assert_eq!(p, vec![vec![folder("새 폴더", &["a", "c"]), app("b")]], "in the target's place, the target first");
        assert_eq!(combine(&mut p, Pos::new(0, 0), app("d"), "x"), Ok(()));
        assert_eq!(p[0][0], folder("새 폴더", &["a", "c", "d"]), "into the folder, its name kept");
        // A folder onto anything, onto nothing, an app onto itself: no.
        assert_eq!(combine(&mut p, Pos::new(0, 1), folder("F", &["x", "y"]), "n"), Err(folder("F", &["x", "y"])));
        assert_eq!(combine(&mut p, Pos::new(0, 9), app("e"), "n"), Err(app("e")));
        assert_eq!(combine(&mut p, Pos::new(0, 1), app("b"), "n"), Err(app("b")));
        assert_eq!(combine(&mut p, Pos::new(0, 0), app("a"), "n"), Ok(()), "already in: nothing twice");
        assert_eq!(p[0][0], folder("새 폴더", &["a", "c", "d"]));
    }

    #[test]
    fn folders_are_found_by_their_session_id() {
        let mut p = vec![vec![app("a"), app("b")], vec![app("c")]];
        combine(&mut p, Pos::new(1, 0), app("d"), "F").unwrap();
        let LpItem::Folder { id, .. } = p[1][0] else { panic!() };
        assert_ne!(id, 0);
        assert_eq!(find(&p, id), Some(Pos::new(1, 0)));
        p[1].insert(0, app("e"));
        assert_eq!(find(&p, id), Some(Pos::new(1, 1)), "moved: found where it is");
        take_out(&mut p, Pos::new(1, 1), 0);
        assert_eq!(find(&p, id), None, "dissolved: gone");
        assert_ne!(new_id(), new_id());
    }

    #[test]
    fn dragging_out_and_dissolving() {
        let mut p = vec![vec![app("x"), folder("F", &["a", "b", "c"])]];
        assert_eq!(take_out(&mut p, Pos::new(0, 1), 1), Some("b".into()));
        assert_eq!(p, vec![vec![app("x"), folder("F", &["a", "c"])]]);
        assert_eq!(take_out(&mut p, Pos::new(0, 1), 0), Some("a".into()));
        assert_eq!(p, vec![vec![app("x"), app("c")]], "one left: the app again, in the folder's place");
        assert_eq!(take_out(&mut p, Pos::new(0, 1), 0), None, "not a folder");
        assert_eq!(take_out(&mut p, Pos::new(0, 7), 0), None);
        let mut v = vec!["a".to_string(), "b".into(), "c".into()];
        move_in(&mut v, 0, 2);
        assert_eq!(v, ["b", "c", "a"]);
        move_in(&mut v, 2, 9);
        assert_eq!(v, ["b", "c", "a"]);
        move_in(&mut v, 9, 0);
        assert_eq!(v, ["b", "c", "a"]);
    }

    #[test]
    fn names() {
        assert_eq!(shown_name("  ", "New Folder"), "New Folder");
        assert_eq!(shown_name(" Games ", "New Folder"), "Games");
        assert_eq!(clean_name("  a\tb\n "), "a b");
        assert_eq!(clean_name(&"가".repeat(60)).chars().count(), MAX_NAME);
    }

    #[test]
    fn the_mini_grid() {
        let ten: Vec<String> = (0..10).map(|i| i.to_string()).collect();
        assert_eq!(mini(&ten).len(), MINI);
        assert_eq!(mini(&ten[..2]).len(), 2);
        let side = 64.0;
        let first = mini_rect(0, side, false);
        let last = mini_rect(MINI - 1, side, false);
        assert!(first.l > 0.0 && first.t > 0.0 && last.r < side && last.b < side, "inside the tile");
        assert!((first.w() - first.h()).abs() < 0.01);
        assert!(mini_rect(1, side, false).l > first.r, "a gap between");
        assert_eq!(mini_rect(0, side, true), mini_rect(MINI_SIDE - 1, side, false), "mirrored");
    }

    #[test]
    fn the_panel_fits_the_screen() {
        let g = super::super::grid::grid(1920.0, 1048.0, super::super::grid::LABEL_H);
        let pg = panel_grid(2, &g, 1920.0, 1048.0);
        assert_eq!((pg.cols, pg.rows), (3, 1), "at least three wide");
        let pg = panel_grid(12, &g, 1920.0, 1048.0);
        assert_eq!((pg.cols, pg.rows), (5, 3));
        let pg = panel_grid(40, &g, 1920.0, 1048.0);
        assert_eq!((pg.cols, pg.rows), (5, g.rows - 1), "rows capped");
        assert_eq!(max_scroll(40, &pg), 8 - pg.rows);
        assert_eq!(max_scroll(4, &panel_grid(4, &g, 1920.0, 1048.0)), 0);
        // A small screen: never wider or taller than the main grid.
        let small = super::super::grid::grid(1024.0, 560.0, super::super::grid::LABEL_H);
        let pg = panel_grid(30, &small, 1024.0, 560.0);
        assert!(pg.cols <= small.cols && pg.rows < small.rows.max(2));
        for (w, h) in [(1920.0, 1048.0), (1024.0, 560.0), (1366.0, 666.0)] {
            let g = super::super::grid::grid(w, h, super::super::grid::LABEL_H);
            let r = panel_rect(&panel_grid(9, &g, w, h));
            assert!(r.l >= 0.0 && r.r <= w && r.t >= 0.0 && r.b <= h, "{w}x{h}: {r:?}");
        }
    }

    #[test]
    fn the_panel_keeps_the_big_plates_rhythm() {
        // 16 from its edge to what it holds, text 28 in (3.3).
        assert_eq!((PANEL_PAD, PANEL_INK), (16.0, 28.0));
        let g = super::super::grid::grid(1920.0, 1048.0, super::super::grid::LABEL_H);
        let pg = panel_grid(9, &g, 1920.0, 1048.0);
        let r = panel_rect(&pg);
        assert_eq!((pg.left - r.l, r.b - (pg.top + pg.cell_h * pg.rows as f32)), (PANEL_PAD, PANEL_PAD));
        // In the middle of the work area.
        assert!((r.cy() - 1048.0 / 2.0).abs() < 0.01 && (r.cx() - 1920.0 / 2.0).abs() < 0.01, "{r:?}");
        // The name's row: 16 in, a field's height, 8 above the apps.
        let n = name_rect(r.w());
        assert_eq!((n.l, n.t, r.w() - n.r, n.h()), (PANEL_PAD, PANEL_PAD, PANEL_PAD, NAME_H));
        assert_eq!(r.t + n.b + 8.0, pg.top);
    }
}
