//! Launchpad's arrangement (pure): the pages and folders kept in the
//! config (`launchpad.layout`, crate::config::LaunchpadCfg) merged with the
//! apps installed now, and fitted to the grid of the monitor it opens on.
//!
//! - Nothing saved: every app in name order, a page at a time.
//! - Saved: its order; apps gone since drop out (from folders too; a folder
//!   left with one app is that app again, an empty one goes); an app listed
//!   twice keeps its first place; apps installed since go after the last
//!   page's apps, in name order.
//! - A page with more apps than this grid holds goes on over the next
//!   pages (a smaller screen than the one it was arranged on); empty pages
//!   go. There is always at least one page.
//!
//! With the App Library (config schema LP_LIBRARY_SCHEMA) the saved pages
//! are Home exactly: apps installed since are not added (they are in the
//! Library, new), hidden apps never show. The first open with the whole
//! app list read migrates an older config: the pages as they were shown
//! (new apps appended, as before) become Home, so nothing moves (migrate).
//! Also here: adding to / removing from Home, hiding and unhiding, and
//! which apps are new.

use std::collections::HashSet;

use crate::config::{LaunchpadCfg, LpItem, LP_LIBRARY_SCHEMA};

pub type Pages = Vec<Vec<LpItem>>;

/// Home's pages: `saved` with the installed apps (`installed`, name order)
/// that are not hidden. `append_new`: apps on no page go after the last
/// page's apps (the old way: no Library, or a config not migrated yet);
/// else they are left out (they are in the Library).
pub fn home(saved: &[Vec<LpItem>], installed: &[String], hidden: &[String], append_new: bool, per_page: usize) -> Pages {
    let hide: HashSet<&str> = hidden.iter().map(|s| s.as_str()).collect();
    let shown: Vec<String> = installed.iter().filter(|id| !hide.contains(id.as_str())).cloned().collect();
    if append_new {
        return arrange(saved, &shown, per_page);
    }
    // Arranged with nothing new: every app already placed counts as placed.
    let placed: HashSet<&str> = saved.iter().flatten().flat_map(|it| match it {
        LpItem::App(id) => vec![id.as_str()],
        LpItem::Folder { apps, .. } => apps.iter().map(|a| a.as_str()).collect(),
    }).collect();
    let only: Vec<String> = shown.into_iter().filter(|id| placed.contains(id.as_str())).collect();
    arrange(saved, &only, per_page)
}

/// Does a config need migrating (an older one, schema 0)?
pub fn needs_migration(cfg: &LaunchpadCfg) -> bool {
    cfg.schema < LP_LIBRARY_SCHEMA
}

/// Migrates an older config, with the whole app list read (`installed`,
/// name order; never with part of it: the apps it lacks would lose their
/// places). The saved pages stay exactly as saved (page breaks, folders,
/// ids not installed now: fitting to a screen and dropping what is not
/// installed stay display-only, in `home`); what the migration changes is
/// only what an older config showed without saving: the installed apps on
/// no page (not hidden) go after the last saved page's items, as they were
/// shown, so nothing moves on the user. Every app installed now is seen
/// (none is new). None: nothing to migrate.
///
/// Home never arranged (nothing saved: every app was shown by name): the
/// Home made here is NopBar's own, so what `clutter` names (uninstallers,
/// web links, documents) is left in the Library instead. A Home the user
/// arranged even once keeps every item as saved. The schema makes this
/// happen once, whatever else is set later.
pub fn migrate(cfg: &LaunchpadCfg, installed: &[String], clutter: &dyn Fn(&str) -> bool) -> Option<LaunchpadCfg> {
    if !needs_migration(cfg) {
        return None;
    }
    let placed: HashSet<&str> = cfg.layout.iter().flatten().flat_map(|it| match it {
        LpItem::App(id) => vec![id.as_str()],
        LpItem::Folder { apps, .. } => apps.iter().map(|a| a.as_str()).collect(),
    }).collect();
    let hide: HashSet<&str> = cfg.hidden.iter().map(|s| s.as_str()).collect();
    let arranged = !placed.is_empty();
    let mut once = HashSet::new();
    let unplaced: Vec<LpItem> = installed
        .iter()
        .filter(|id| !id.is_empty() && !placed.contains(id.as_str()) && !hide.contains(id.as_str()) && (arranged || !clutter(id)) && once.insert(id.as_str()))
        .map(|id| LpItem::App(id.clone()))
        .collect();
    let mut pages = cfg.layout.clone();
    if !unplaced.is_empty() {
        match pages.iter_mut().rev().find(|p| !p.is_empty()) {
            Some(last) => last.extend(unplaced),
            None => pages = vec![unplaced],
        }
    }
    let mut seen: Vec<String> = Vec::new();
    let mut once = HashSet::new();
    for id in installed {
        if once.insert(id.as_str()) {
            seen.push(id.clone());
        }
    }
    Some(LaunchpadCfg { layout: pages, schema: LP_LIBRARY_SCHEMA, seen, ..cfg.clone() })
}

/// The installed apps Launchpad has not seen yet (new: a dot in the
/// Library), in `installed`'s order.
pub fn new_apps(installed: &[String], seen: &[String]) -> Vec<String> {
    let s: HashSet<&str> = seen.iter().map(|x| x.as_str()).collect();
    let mut once = HashSet::new();
    installed.iter().filter(|id| !s.contains(id.as_str()) && once.insert(id.as_str())).cloned().collect()
}

/// App `id` was opened or moved: it is not new any more. True: changed.
pub fn mark_seen(seen: &mut Vec<String>, id: &str) -> bool {
    if id.is_empty() || seen.iter().any(|s| s == id) {
        return false;
    }
    seen.push(id.to_string());
    true
}

/// Is app `id` on Home (on a page or in a folder)?
pub fn on_home(pages: &[Vec<LpItem>], id: &str) -> bool {
    pages.iter().flatten().any(|it| match it {
        LpItem::App(a) => a == id,
        LpItem::Folder { apps, .. } => apps.iter().any(|a| a == id),
    })
}

/// The app ids on Home, in its order (folders' apps where the folder is).
pub fn home_ids(pages: &[Vec<LpItem>]) -> Vec<String> {
    pages
        .iter()
        .flatten()
        .flat_map(|it| match it {
            LpItem::App(id) => vec![id.clone()],
            LpItem::Folder { apps, .. } => apps.clone(),
        })
        .collect()
}

/// Takes app `id` off Home (from a page or a folder: a folder left with one
/// app is that app again, an empty one goes; empty pages go, one stays).
/// True: it was there.
pub fn remove_from_home(pages: &mut Pages, id: &str) -> bool {
    let mut found = false;
    for page in pages.iter_mut() {
        let mut out = Vec::with_capacity(page.len());
        for it in std::mem::take(page) {
            match it {
                LpItem::App(a) if a == id => found = true,
                LpItem::Folder { name, mut apps, id: fid, auto } => {
                    let before = apps.len();
                    apps.retain(|a| a != id);
                    let took = apps.len() != before;
                    found |= took;
                    match apps.len() {
                        0 => {}
                        1 => out.push(LpItem::App(apps.remove(0))),
                        // (An app taken out by the user: the folder is theirs now.)
                        _ => out.push(LpItem::Folder { name, apps, id: fid, auto: auto && !took }),
                    }
                }
                other => out.push(other),
            }
        }
        *page = out;
    }
    if found {
        pages.retain(|p| !p.is_empty());
        if pages.is_empty() {
            pages.push(Vec::new());
        }
    }
    found
}

/// Puts app `id` on Home after the last page's apps (a new page when that
/// one is full). False: it is there already.
pub fn add_to_home(pages: &mut Pages, id: &str, per_page: usize) -> bool {
    if id.is_empty() || on_home(pages, id) {
        return false;
    }
    let pp = per_page.max(1);
    pages.retain(|p| !p.is_empty());
    match pages.last_mut() {
        Some(last) if last.len() < pp => last.push(LpItem::App(id.to_string())),
        _ => pages.push(vec![LpItem::App(id.to_string())]),
    }
    true
}

/// Hides app `id`: off Home, on the hidden list. True: changed.
pub fn hide(pages: &mut Pages, hidden: &mut Vec<String>, id: &str) -> bool {
    let off = remove_from_home(pages, id);
    let new = !id.is_empty() && !hidden.iter().any(|h| h == id);
    if new {
        hidden.push(id.to_string());
    }
    off || new
}

/// Unhides app `id` (it goes back to its category in the Library, not onto
/// Home). True: it was hidden.
pub fn unhide(hidden: &mut Vec<String>, id: &str) -> bool {
    let before = hidden.len();
    hidden.retain(|h| h != id);
    hidden.len() != before
}

/// The pages to show: `saved` merged with `installed` (app ids in name
/// order), at most `per_page` items a page.
pub fn arrange(saved: &[Vec<LpItem>], installed: &[String], per_page: usize) -> Pages {
    let have: HashSet<&str> = installed.iter().map(|s| s.as_str()).collect();
    let mut seen: HashSet<String> = HashSet::new();
    let mut pages: Pages = Vec::new();
    for page in saved {
        let mut out = Vec::new();
        for item in page {
            match item {
                LpItem::App(id) => {
                    if have.contains(id.as_str()) && seen.insert(id.clone()) {
                        out.push(LpItem::App(id.clone()));
                    }
                }
                LpItem::Folder { name, apps, id, auto } => {
                    let kept: Vec<String> = apps.iter().filter(|a| have.contains(a.as_str()) && seen.insert((*a).clone())).cloned().collect();
                    match kept.len() {
                        0 => {}
                        1 => out.push(LpItem::App(kept[0].clone())),
                        // (Its session id kept; a new one if it had none.)
                        // (Apps uninstalled since are no change of the user's: the mark stays.)
                        _ => out.push(LpItem::Folder { name: name.clone(), apps: kept, id: if *id == 0 { super::folders::new_id() } else { *id }, auto: *auto }),
                    }
                }
            }
        }
        pages.push(out);
    }
    // (An id listed twice by the shell goes in once.)
    let new: Vec<LpItem> = installed.iter().filter(|id| seen.insert((*id).clone())).map(|id| LpItem::App(id.clone())).collect();
    match pages.iter_mut().rev().find(|p| !p.is_empty()) {
        Some(last) => last.extend(new),
        None => pages = vec![new],
    }
    fit(pages, per_page)
}

/// Pages of at most `per_page` items (an overfull one goes on over new
/// pages after it), no empty page, at least one page.
pub fn fit(pages: Pages, per_page: usize) -> Pages {
    let pp = per_page.max(1);
    let mut out: Pages = Vec::new();
    for page in pages {
        let mut rest = page;
        while !rest.is_empty() {
            let tail = if rest.len() > pp { rest.split_off(pp) } else { Vec::new() };
            out.push(rest);
            rest = tail;
        }
    }
    if out.is_empty() {
        out.push(Vec::new());
    }
    out
}

/// What is saved for `pages`: as they are (one empty page: nothing).
pub fn to_config(pages: &Pages) -> Vec<Vec<LpItem>> {
    pages.iter().filter(|p| !p.is_empty()).cloned().collect()
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
    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn nothing_saved_is_name_order_by_page() {
        let installed = ids(&["a", "b", "c", "d", "e"]);
        assert_eq!(arrange(&[], &installed, 2), vec![vec![app("a"), app("b")], vec![app("c"), app("d")], vec![app("e")]]);
        assert_eq!(arrange(&[], &[], 35), vec![Vec::<LpItem>::new()], "no apps: one empty page");
        assert_eq!(arrange(&[vec![], vec![]], &ids(&["a"]), 35), vec![vec![app("a")]], "saved but empty pages");
    }

    #[test]
    fn saved_order_is_kept_and_new_apps_go_last() {
        let saved = vec![vec![app("c"), folder("F", &["a", "d"])], vec![app("b")]];
        let installed = ids(&["a", "b", "c", "d", "e", "f"]);
        let got = arrange(&saved, &installed, 35);
        assert_eq!(got, vec![vec![app("c"), folder("F", &["a", "d"])], vec![app("b"), app("e"), app("f")]]);
        // The last page full: new apps start a page.
        let got = arrange(&saved, &installed, 1);
        assert_eq!(got, vec![vec![app("c")], vec![folder("F", &["a", "d"])], vec![app("b")], vec![app("e")], vec![app("f")]]);
        // New apps after a trailing empty page land on the last page with apps.
        let got = arrange(&[vec![app("a")], vec![]], &ids(&["a", "z"]), 35);
        assert_eq!(got, vec![vec![app("a"), app("z")]]);
    }

    #[test]
    fn uninstalled_apps_drop_out_cleanly() {
        let saved = vec![vec![app("gone"), app("a"), folder("Two", &["b", "gone2"]), folder("None", &["x", "y"]), folder("Keep", &["c", "d", "gone"])], vec![app("gone3")]];
        let installed = ids(&["a", "b", "c", "d"]);
        let got = arrange(&saved, &installed, 35);
        assert_eq!(got, vec![vec![app("a"), app("b"), folder("Keep", &["c", "d"])]], "one left: the app; none left: gone; the empty page too");
    }

    #[test]
    fn duplicates_keep_their_first_place() {
        let saved = vec![vec![app("a"), folder("F", &["a", "b", "c"]), app("b")]];
        let got = arrange(&saved, &ids(&["a", "b", "c"]), 35);
        assert_eq!(got, vec![vec![app("a"), folder("F", &["b", "c"])]]);
        // Unreadable entries (no id) never show.
        let got = arrange(&[vec![app(""), app("a")]], &ids(&["a"]), 35);
        assert_eq!(got, vec![vec![app("a")]]);
    }

    #[test]
    fn new_ids_listed_twice_go_in_once() {
        // Nothing saved.
        let got = arrange(&[], &ids(&["a", "a", "b", "b", "a"]), 35);
        assert_eq!(got, vec![vec![app("a"), app("b")]]);
        // Saved, with new apps listed twice (and one already placed).
        let saved = vec![vec![app("x"), folder("F", &["y", "z"])]];
        let got = arrange(&saved, &ids(&["x", "y", "z", "n", "n", "x", "y"]), 35);
        assert_eq!(got, vec![vec![app("x"), folder("F", &["y", "z"]), app("n")]]);
    }

    #[test]
    fn folders_get_and_keep_a_session_id() {
        let first = arrange(&[vec![folder("F", &["a", "b"]), folder("G", &["c", "d"])]], &ids(&["a", "b", "c", "d"]), 35);
        let id = |p: &Pages, k: usize| match &p[0][k] {
            LpItem::Folder { id, .. } => *id,
            _ => 0,
        };
        assert!(id(&first, 0) != 0 && id(&first, 1) != 0 && id(&first, 0) != id(&first, 1), "given, and each its own");
        // Arranged again from the pages as they are: the same ids.
        let again = arrange(&to_config(&first), &ids(&["a", "b", "c", "d", "e"]), 35);
        assert_eq!((id(&again, 0), id(&again, 1)), (id(&first, 0), id(&first, 1)));
    }

    #[test]
    fn overfull_pages_go_on_over_new_pages() {
        let saved = vec![vec![app("a"), app("b"), app("c")], vec![app("d")]];
        let got = arrange(&saved, &ids(&["a", "b", "c", "d"]), 2);
        assert_eq!(got, vec![vec![app("a"), app("b")], vec![app("c")], vec![app("d")]]);
        assert_eq!(fit(vec![], 3), vec![Vec::<LpItem>::new()]);
        assert_eq!(fit(vec![vec![app("a")]], 0), vec![vec![app("a")]], "a zero grid still shows the app");
    }

    /// No app looks like clutter.
    fn none(_: &str) -> bool {
        false
    }

    #[test]
    fn migration_keeps_what_was_shown_as_home() {
        // An older config: arranged pages, apps installed since appended.
        let old = LaunchpadCfg { layout: vec![vec![app("c"), folder("F", &["a", "d"])]], ..Default::default() };
        assert!(needs_migration(&old));
        let installed = ids(&["a", "b", "c", "d", "e"]);
        let shown_before = arrange(&old.layout, &installed, 35);
        let m = migrate(&old, &installed, &none).expect("migrated");
        assert_eq!(m.schema, LP_LIBRARY_SCHEMA);
        assert_eq!(home(&m.layout, &installed, &m.hidden, false, 35), shown_before, "nothing moved on the user");
        assert_eq!(m.seen, installed, "nothing is new");
        assert!(migrate(&m, &installed, &none).is_none(), "once");
        // Never arranged (every app by name): all of them become Home, fitted
        // to a screen only when shown.
        let m = migrate(&LaunchpadCfg::default(), &installed, &none).unwrap();
        assert_eq!(home_ids(&m.layout), installed);
        assert_eq!(m.layout.len(), 1, "not split by the grid of the screen it first opened on");
        assert_eq!(home(&m.layout, &installed, &[], false, 2).len(), 3, "split when shown");
        // Hidden apps are left out even then; other fields are kept.
        let c = LaunchpadCfg { hidden: vec!["b".into()], show_library: false, ..Default::default() };
        let m = migrate(&c, &installed, &none).unwrap();
        assert!(!on_home(&m.layout, "b") && !m.show_library && m.hidden == vec!["b".to_string()]);
    }

    #[test]
    fn migration_keeps_the_saved_structure_as_saved() {
        // A page of 30 (arranged on a big screen), an id not installed now,
        // a folder with one installed app and one gone, an empty page.
        let mut big: Vec<LpItem> = (0..30).map(|i| app(&format!("a{i}"))).collect();
        big.push(app("gone"));
        let saved = vec![big, vec![folder("F", &["b", "away"]), app("c")], vec![]];
        let mut installed: Vec<String> = (0..30).map(|i| format!("a{i}")).collect();
        installed.extend(ids(&["b", "c"]));
        let old = LaunchpadCfg { layout: saved.clone(), ..Default::default() };
        let m = migrate(&old, &installed, &none).unwrap();
        assert_eq!(serde_json::to_string(&m.layout).unwrap(), serde_json::to_string(&saved).unwrap(), "byte for byte: nothing installed is unplaced");
        // Shown on a small screen: fitted and filtered there only.
        let shown = home(&m.layout, &installed, &[], false, 21);
        assert_eq!(shown[0].len(), 21);
        assert_eq!(shown.iter().find_map(|p| p.iter().find(|it| matches!(it, LpItem::App(a) if a == "b"))).is_some(), true, "a one-app folder shows as the app");
        assert!(!on_home(&shown, "gone"));
        // Saved again unchanged: back on the big screen, the 30 are together.
        assert!(on_home(&m.layout, "gone") && on_home(&m.layout, "away"));
        // The one thing it changes: apps shown appended before are saved there.
        let with_new = [installed.clone(), ids(&["n"])].concat();
        let m = migrate(&old, &with_new, &none).unwrap();
        assert_eq!(&m.layout[..1], &saved[..1]);
        assert_eq!(m.layout[1], vec![folder("F", &["b", "away"]), app("c"), app("n")], "after the last saved page's items");
        assert!(m.layout[2].is_empty(), "the rest as saved");
    }

    #[test]
    fn a_home_never_arranged_starts_without_clutter_an_arranged_one_is_kept() {
        let installed = ids(&["a", "d", "u", "w"]);
        let clutter = |id: &str| matches!(id, "u" | "w" | "d");
        // Nothing saved (every app by name): NopBar's own Home leaves them in
        // the Library; seen all the same (not new).
        let m = migrate(&LaunchpadCfg::default(), &installed, &clutter).unwrap();
        assert_eq!(home_ids(&m.layout), ids(&["a"]));
        assert_eq!(m.seen, installed);
        assert!(migrate(&m, &installed, &clutter).is_none(), "once: the schema says it is done");
        // Arranged by the user, even with clutter in a folder of theirs and
        // on a page: every item stays exactly as saved, and the apps shown
        // after them are kept too (nothing moves on the user).
        let saved = vec![vec![folder("Work", &["a", "d"])], vec![app("u")]];
        let old = LaunchpadCfg { layout: saved.clone(), ..Default::default() };
        let m = migrate(&old, &installed, &clutter).unwrap();
        assert_eq!(&m.layout[..1], &saved[..1], "the user's folder as saved");
        assert_eq!(m.layout[1], vec![app("u"), app("w")]);
        // The old "Not now" flag changes nothing either way.
        for dismissed in [false, true] {
            let c = LaunchpadCfg { cleanup_dismissed: dismissed, ..Default::default() };
            assert_eq!(home_ids(&migrate(&c, &installed, &clutter).unwrap().layout), ids(&["a"]), "{dismissed}");
            let c = LaunchpadCfg { cleanup_dismissed: dismissed, layout: saved.clone(), ..Default::default() };
            assert!(on_home(&migrate(&c, &installed, &clutter).unwrap().layout, "d"), "{dismissed}");
        }
    }

    #[test]
    fn after_the_migration_new_apps_go_to_the_library_only() {
        let saved = vec![vec![app("a"), folder("F", &["b", "c"])]];
        let installed = ids(&["a", "b", "c", "n1", "n2"]);
        let h = home(&saved, &installed, &[], false, 35);
        assert_eq!(h, vec![vec![app("a"), folder("F", &["b", "c"])]], "new apps are not added");
        assert_eq!(new_apps(&installed, &ids(&["a", "b", "c"])), ids(&["n1", "n2"]));
        // The Library off: appended as before, hidden apps still out.
        let h = home(&saved, &installed, &ids(&["n2", "a"]), true, 35);
        assert_eq!(h, vec![vec![folder("F", &["b", "c"]), app("n1")]]);
        // Uninstalled apps still drop out.
        let h = home(&saved, &ids(&["a", "b"]), &[], false, 35);
        assert_eq!(h, vec![vec![app("a"), app("b")]]);
        let mut seen = ids(&["a"]);
        assert!(mark_seen(&mut seen, "n1") && !mark_seen(&mut seen, "n1") && !mark_seen(&mut seen, ""));
        assert_eq!(new_apps(&installed, &seen), ids(&["b", "c", "n2"]));
        assert_eq!(new_apps(&ids(&["x", "x"]), &[]), ids(&["x"]));
    }

    #[test]
    fn hide_unhide_add_remove() {
        let mut p = vec![vec![app("a"), folder("F", &["b", "c"])], vec![app("d")]];
        let mut hidden = Vec::new();
        // Remove from Home: from a folder (one left: the app), from a page (empty page goes).
        assert!(remove_from_home(&mut p, "b"));
        assert_eq!(p, vec![vec![app("a"), app("c")], vec![app("d")]]);
        assert!(remove_from_home(&mut p, "d"));
        assert_eq!(p, vec![vec![app("a"), app("c")]]);
        assert!(!remove_from_home(&mut p, "zz"), "not there");
        // Add to Home: at the end, a new page when full, never twice.
        assert!(add_to_home(&mut p, "d", 3));
        assert_eq!(p, vec![vec![app("a"), app("c"), app("d")]]);
        assert!(add_to_home(&mut p, "b", 3));
        assert_eq!(p, vec![vec![app("a"), app("c"), app("d")], vec![app("b")]]);
        assert!(!add_to_home(&mut p, "b", 3) && !add_to_home(&mut p, "", 3));
        // Hide: off Home and listed; again: nothing; unhide: listed no more, not back on Home.
        assert!(hide(&mut p, &mut hidden, "c"));
        assert!(!on_home(&p, "c") && hidden == vec!["c".to_string()]);
        assert!(!hide(&mut p, &mut hidden, "c"));
        assert!(unhide(&mut hidden, "c") && hidden.is_empty() && !on_home(&p, "c"));
        assert!(!unhide(&mut hidden, "c"));
        // A hidden app not on Home (in the Library) is just listed.
        assert!(hide(&mut p, &mut hidden, "lib-only"));
        // Removing the last app: one empty page stays.
        let mut q = vec![vec![app("x")]];
        assert!(remove_from_home(&mut q, "x"));
        assert_eq!(q, vec![Vec::<LpItem>::new()]);
        assert!(add_to_home(&mut q, "y", 3));
        assert_eq!(q, vec![vec![app("y")]]);
    }

    #[test]
    fn saving() {
        let pages = vec![vec![app("a"), folder("F", &["b", "c"])], vec![], vec![app("d")]];
        assert_eq!(to_config(&pages), vec![vec![app("a"), folder("F", &["b", "c"])], vec![app("d")]]);
        assert!(to_config(&vec![vec![]]).is_empty(), "nothing on it: nothing saved");
    }
}
