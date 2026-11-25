//! "Tidy Home" (pure): Home arranged in one go. The first page is the
//! most-used apps (as many as a page holds, by usage; with no usage known,
//! in the order they are in now); the folders the user made follow, as
//! they are; then the other apps, one folder per App Library category
//! (named by the category; a category with one app leaves it loose).
//!
//! Only a folder marked as made by tidying (LpItem::Folder::auto, saved)
//! is opened up and sorted again, so tidying twice gives the same Home and
//! its apps can still rise to the first page. Every other folder is the
//! user's and kept whole: one they made, one from an older file (never
//! marked), and one tidying made that they changed since (renamed, an app
//! put in or taken out: the mark goes, folders.rs, layout_store.rs,
//! picks.rs). A name is never taken as a sign.

use std::collections::{BTreeMap, HashSet};

use super::layout_store::{self, Pages};
use super::library::Cat;
use crate::config::LpItem;

/// What Home and the Library are at a moment (Home as saved, the hidden
/// apps, the user's categories): what a tidy left, to tell whether
/// anything changed since.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mark {
    pub home: Pages,
    pub hidden: Vec<String>,
    pub library: BTreeMap<String, String>,
}

/// Home before the last tidy, for Ctrl+Z: good only while nothing else
/// changed since the tidy (any edit forgets it; and it is checked against
/// what the tidy left), given back once, kept in memory only while
/// Launchpad is open.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Undo {
    kept: Option<(Pages, Mark)>,
}

impl Undo {
    /// Home was tidied from `before` and left as `after` (a newer tidy
    /// replaces an older one).
    pub fn remember(&mut self, before: Pages, after: Mark) {
        self.kept = Some((before, after));
    }

    /// Something else was changed: nothing to undo any more.
    pub fn forget(&mut self) {
        self.kept = None;
    }

    /// Is there something to undo?
    pub fn armed(&self) -> bool {
        self.kept.is_some()
    }

    /// Ctrl+Z with things as they are `now`: Home as it was before the
    /// tidy if nothing changed since; None otherwise (and it is gone
    /// either way: once).
    pub fn take(&mut self, now: &Mark) -> Option<Pages> {
        let (before, after) = self.kept.take()?;
        (after == *now).then_some(before)
    }
}

/// Home tidied: `home` (its pages now), `per_page` items a page, each app's
/// `score` (its use; 0 unknown), its category (`cat`) and the categories'
/// names (`cat_name`: the new folders' names, never read back).
pub fn tidy(home: &Pages, per_page: usize, score: &dyn Fn(&str) -> f64, cat: &dyn Fn(&str) -> Cat, cat_name: &dyn Fn(Cat) -> String) -> Pages {
    let pp = per_page.max(1);
    // Loose apps in their order now (with those of folders tidying made);
    // the user's folders as they are.
    let mut loose: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut kept: Vec<LpItem> = Vec::new();
    for it in home.iter().flatten() {
        match it {
            LpItem::App(id) => {
                if !id.is_empty() && seen.insert(id.clone()) {
                    loose.push(id.clone());
                }
            }
            LpItem::Folder { apps, auto: true, .. } => {
                loose.extend(apps.iter().filter(|a| seen.insert((*a).clone())).cloned());
            }
            folder => {
                if let LpItem::Folder { apps, .. } = folder {
                    seen.extend(apps.iter().cloned());
                }
                kept.push(folder.clone());
            }
        }
    }
    // By use, most first (a tie, or nothing known: the order now).
    let mut ranked: Vec<(usize, &String)> = loose.iter().enumerate().collect();
    ranked.sort_by(|(i, a), (j, b)| score(b).partial_cmp(&score(a)).unwrap_or(std::cmp::Ordering::Equal).then(i.cmp(j)));
    let first: Vec<LpItem> = ranked.iter().take(pp).map(|(_, id)| LpItem::App((*id).clone())).collect();
    let mut rest: Vec<LpItem> = kept;
    for c in Cat::ALL {
        let apps: Vec<String> = ranked.iter().skip(pp).filter(|(_, id)| cat(id) == c).map(|(_, id)| (*id).clone()).collect();
        match apps.len() {
            0 => {}
            1 => rest.push(LpItem::App(apps[0].clone())),
            _ => rest.push(LpItem::Folder { name: cat_name(c), apps, id: super::folders::new_id(), auto: true }),
        }
    }
    let mut pages: Pages = Vec::new();
    if !first.is_empty() {
        pages.push(first);
    }
    pages.extend(layout_store::fit(vec![rest], pp).into_iter().filter(|p| !p.is_empty()));
    if pages.is_empty() {
        pages.push(Vec::new());
    }
    pages
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn app(id: &str) -> LpItem {
        LpItem::App(id.into())
    }
    fn folder(name: &str, apps: &[&str]) -> LpItem {
        LpItem::folder(name, apps.iter().map(|s| s.to_string()).collect())
    }
    /// A folder Tidy Home made (marked).
    fn auto(name: &str, apps: &[&str]) -> LpItem {
        LpItem::Folder { name: name.into(), apps: apps.iter().map(|s| s.to_string()).collect(), id: 0, auto: true }
    }
    /// Categories by the id's first letter: g games, d dev, o office, else other.
    fn cat(id: &str) -> Cat {
        match id.chars().next() {
            Some('g') => Cat::Games,
            Some('d') => Cat::Dev,
            Some('o') => Cat::Office,
            _ => Cat::Other,
        }
    }
    fn name(c: Cat) -> String {
        format!("{c:?}")
    }
    fn scores(v: &[(&str, f64)]) -> HashMap<String, f64> {
        v.iter().map(|(k, s)| (k.to_string(), *s)).collect()
    }
    fn run(home: &Pages, pp: usize, s: &HashMap<String, f64>) -> Pages {
        tidy(home, pp, &|id| s.get(id).copied().unwrap_or(0.0), &cat, &name)
    }

    #[test]
    fn the_first_page_is_the_most_used() {
        let home = vec![vec![app("g1"), app("d1"), app("o1"), app("x1")], vec![app("g2"), app("d2"), app("g3")]];
        let s = scores(&[("g3", 9.0), ("o1", 5.0), ("d2", 5.0), ("x1", 1.0)]);
        let got = run(&home, 3, &s);
        assert_eq!(got[0], vec![app("g3"), app("o1"), app("d2")], "by use; a tie keeps the order now");
        // The rest: one folder per category, a lone app loose, by the
        // Library's order (Dev, Games, ..., Other).
        assert_eq!(got[1], vec![app("d1"), auto("Games", &["g1", "g2"]), app("x1")], "marked as tidy's");
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn nothing_known_keeps_the_order_for_the_first_page() {
        let home = vec![vec![app("x1"), app("g1"), app("d1"), app("g2"), app("g3"), app("d2")]];
        let got = run(&home, 2, &HashMap::new());
        assert_eq!(got[0], vec![app("x1"), app("g1")]);
        assert_eq!(got[1], vec![auto("Dev", &["d1", "d2"]), auto("Games", &["g2", "g3"])]);
    }

    #[test]
    fn the_users_folders_are_kept_whole_after_the_first_page() {
        let mine = folder("Work stuff", &["o1", "g9"]);
        let home = vec![vec![app("g1"), mine.clone(), app("g2")], vec![app("o2"), app("g3")]];
        let s = scores(&[("g9", 100.0), ("g3", 2.0)]);
        let got = run(&home, 2, &s);
        assert_eq!(got[0], vec![app("g3"), app("g1")], "an app in the user's folder stays in it");
        assert_eq!(got[1], vec![mine, app("g2")], "their folder first, then the categories");
        assert_eq!(got[2], vec![app("o2")]);
        assert!(got.iter().all(|p| p.len() <= 2));
    }

    #[test]
    fn tidying_twice_gives_the_same_home() {
        let home = vec![vec![app("g1"), app("g2"), app("g3"), app("d1"), app("d2"), app("o1"), app("x1")]];
        let s = scores(&[("o1", 3.0)]);
        let once = run(&home, 2, &s);
        assert_eq!(run(&once, 2, &s), once);
        // A tidy folder's app used a lot since rises to the first page.
        let s2 = scores(&[("o1", 3.0), ("g3", 50.0)]);
        let again = run(&once, 2, &s2);
        assert_eq!(again[0], vec![app("g3"), app("o1")]);
        // A tidy folder the user changed (an app put in: the mark goes) is theirs.
        let mut mine = once.clone();
        let at = mine
            .iter()
            .enumerate()
            .find_map(|(p, v)| v.iter().position(|it| matches!(it, LpItem::Folder { name, .. } if name == "Games")).map(|s| super::super::edit::Pos::new(p, s)))
            .unwrap();
        super::super::folders::combine(&mut mine, at, app("d9"), "x").unwrap();
        let got = run(&mine, 2, &s2);
        assert!(got.iter().flatten().any(|it| *it == folder("Games", &["g2", "g3", "d9"])), "kept whole, unmarked: {got:?}");
    }

    /// The review's case: folders the user made, named like a category and
    /// holding only its apps, are theirs (a name is no sign of tidying).
    #[test]
    fn user_folders_named_like_a_category_are_kept() {
        let a = folder("Games", &["g1", "g2"]);
        let b = folder("Games", &["g3", "g4"]);
        let home = vec![vec![a.clone(), app("x1"), b.clone(), app("g5"), app("g6")]];
        let got = run(&home, 1, &scores(&[("g1", 99.0)]));
        assert_eq!(got[0], vec![app("x1")], "g1 stays in their folder however used");
        assert_eq!(got[1..].concat(), vec![a, b, auto("Games", &["g5", "g6"])], "both kept apart, as they were");
    }

    /// A folder an older file has (no mark) is the user's; a marked one is
    /// found by its mark whatever language it was named in.
    #[test]
    fn the_mark_not_the_language_decides() {
        let old = LpItem::from_value(&serde_json::json!({"folder": "Games", "apps": ["g1", "g2"]}));
        assert_eq!(old, folder("Games", &["g1", "g2"]), "no mark in an older file");
        let marked = LpItem::from_value(&serde_json::json!({"folder": "게임", "apps": ["g1", "g2"], "auto": true}));
        assert_eq!(marked, auto("게임", &["g1", "g2"]));
        // Saved and read back with its mark.
        let back = LpItem::from_value(&serde_json::to_value(&marked).unwrap());
        assert_eq!(back, marked);
        assert!(!serde_json::to_string(&old).unwrap().contains("auto"), "a user's folder is saved as before");
        // Named in Korean, tidied in English: still opened up and named anew.
        let got = run(&vec![vec![app("x1"), marked]], 1, &HashMap::new());
        assert_eq!(got, vec![vec![app("x1")], vec![auto("Games", &["g1", "g2"])]]);
        // Anything but a plain true: the user's.
        let odd = LpItem::from_value(&serde_json::json!({"folder": "G", "apps": ["g1", "g2"], "auto": "yes"}));
        assert_eq!(odd, folder("G", &["g1", "g2"]));
    }

    /// Changing a tidy folder takes its mark away (an app taken out by hand
    /// or off Home, one put in); apps uninstalled since leave it.
    #[test]
    fn editing_a_tidy_folder_makes_it_the_users() {
        use super::super::edit::Pos;
        let mut p = vec![vec![auto("Games", &["g1", "g2", "g3"])]];
        super::super::folders::take_out(&mut p, Pos::new(0, 0), 0);
        assert_eq!(p[0][0], folder("Games", &["g2", "g3"]));
        let mut p = vec![vec![auto("Games", &["g1", "g2", "g3"])]];
        layout_store::remove_from_home(&mut p, "g1");
        assert_eq!(p[0][0], folder("Games", &["g2", "g3"]));
        let mut p = vec![vec![auto("Games", &["g1", "g2", "g3"])]];
        layout_store::remove_from_home(&mut p, "zz");
        assert_eq!(p[0][0], auto("Games", &["g1", "g2", "g3"]), "nothing taken: the mark stays");
        let mut p = vec![vec![auto("Games", &["g1", "g2"]), app("g3")]];
        super::super::picks::combine_many(&mut p, Pos::new(0, 0), vec![app("g4")], "x").unwrap();
        assert_eq!(p[0][0], folder("Games", &["g1", "g2", "g4"]));
        let mut p = vec![vec![auto("Games", &["g1", "g2"]), app("a")]];
        super::super::picks::group(&mut p, &[Pos::new(0, 0), Pos::new(0, 1)], "x", 9).unwrap();
        assert_eq!(p[0][0], folder("Games", &["g1", "g2", "a"]), "grouped into: the user's");
        let arranged = layout_store::arrange(&[vec![auto("Games", &["g1", "g2", "gone"])]], &["g1".into(), "g2".into()], 9);
        assert_eq!(arranged[0][0], auto("Games", &["g1", "g2"]), "uninstalled since: still tidy's");
    }

    #[test]
    fn small_and_empty_homes() {
        assert_eq!(run(&vec![vec![]], 4, &HashMap::new()), vec![Vec::<LpItem>::new()]);
        let home = vec![vec![app("g1"), app("d1")]];
        assert_eq!(run(&home, 4, &HashMap::new()), home, "all on the first page: no folders");
        let only = vec![vec![folder("F", &["a", "b"])]];
        assert_eq!(run(&only, 4, &HashMap::new()), only);
        // Every app once, nothing lost or added.
        let home = vec![vec![app("g1"), folder("F", &["a", "b"]), app("g1"), app("d1")], vec![app("o1"), app("o2"), app("o3")]];
        let got = run(&home, 2, &HashMap::new());
        let mut ids = layout_store::home_ids(&got);
        ids.sort();
        assert_eq!(ids, ["a", "b", "d1", "g1", "o1", "o2", "o3"]);
    }

    fn mark(home: &Pages) -> Mark {
        Mark { home: home.clone(), ..Default::default() }
    }

    #[test]
    fn ctrl_z_puts_home_back_once_right_after_the_tidy() {
        let home = vec![vec![app("x1"), app("g1"), app("g2"), app("d1")]];
        let mut undo = Undo::default();
        assert_eq!(undo.take(&mark(&home)), None, "nothing tidied: nothing to undo");
        let tidied = run(&home, 1, &scores(&[("d1", 3.0)]));
        assert_ne!(tidied, home);
        undo.remember(home.clone(), mark(&tidied));
        assert!(undo.armed());
        assert_eq!(undo.take(&mark(&tidied)), Some(home.clone()), "exactly as it was");
        assert_eq!(undo.take(&mark(&tidied)), None, "once");
        // Tidied twice: back to before the last one.
        undo.remember(home.clone(), mark(&tidied));
        undo.remember(tidied.clone(), mark(&home));
        assert_eq!(undo.take(&mark(&home)), Some(tidied));
    }

    /// The review's case: anything done after the tidy (grouped, moved,
    /// added, removed, hidden, a category changed) ends the undo, and
    /// Ctrl+Z never writes over it.
    #[test]
    fn any_edit_after_the_tidy_ends_the_undo() {
        let home = vec![vec![app("x1"), app("g1"), app("g2"), app("d1")]];
        let tidied = run(&home, 1, &HashMap::new());
        let after = mark(&tidied);
        let mut undo = Undo::default();
        // Each edit forgets it (view/organize.rs: edited, commit_home).
        undo.remember(home.clone(), after.clone());
        undo.forget();
        assert!(!undo.armed());
        assert_eq!(undo.take(&after), None);
        // And should one be missed, what differs from what the tidy left is
        // never written over.
        let mut moved = tidied.clone();
        moved.reverse();
        let mut grouped = tidied.clone();
        grouped[0].push(folder("Mine", &["a", "b"]));
        let mut added = tidied.clone();
        added[0].push(app("new"));
        for now in [
            mark(&moved),
            mark(&grouped),
            mark(&added),
            Mark { hidden: vec!["g1".into()], ..after.clone() },
            Mark { library: [("g1".to_string(), "dev".to_string())].into(), ..after.clone() },
        ] {
            undo.remember(home.clone(), after.clone());
            assert_eq!(undo.take(&now), None, "{now:?}");
            assert!(!undo.armed(), "gone after a refused Ctrl+Z too");
        }
    }
}
