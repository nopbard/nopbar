//! Which notes show on which virtual desktop (pure).
//!
//! A note belongs to one of Windows' virtual desktops (its GUID, as the
//! registry names it: crate::vdesk::guid_text) or to every desktop (None).
//! A new note belongs to the desktop it was made on. Notes of other
//! desktops are hidden (their windows kept, never closed: their text is
//! saved like any other) and show again on their desktop.
//!
//! Notes are hidden only when that is sure: crate::vdesk::sure() gives a
//! reading only when every value it rests on was read and every change of
//! them is heard. A failed read, a watch that cannot hear, a watch that
//! never started: not known, and everything shows (a failure shows notes,
//! it never hides them). Settings > widgets > "Each desktop its own notes"
//! turned off shows everything too.
//!
//! A note of a desktop that no longer exists (the user closed it) belongs
//! to every desktop from then on, decided from a sure reading only: two
//! desktops or more without it, or a single desktop that is not it (or
//! Windows listing none: it has only ever had one).

use crate::vdesk::{guid_text, Sure};

/// What is surely known of the virtual desktops (GUID texts).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seen {
    /// Nothing sure: everything shows, nothing moves.
    Unknown,
    /// A single desktop (its GUID, or None when Windows lists none).
    One(Option<String>),
    /// Two or more: the current one and all of them.
    Many { current: String, all: Vec<String> },
}

/// What is known of the virtual desktops now, and the setting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Desks {
    /// "Each desktop its own notes" (Settings > widgets).
    pub per_desktop: bool,
    pub seen: Seen,
}

impl Default for Desks {
    /// Nothing known yet: every note shows.
    fn default() -> Self {
        Desks { per_desktop: true, seen: Seen::Unknown }
    }
}

/// Pure: a stored desktop id as compared here (GUID texts in upper case;
/// anything else is kept as it is and simply matches no desktop).
pub fn norm(id: &str) -> String {
    id.trim().to_ascii_uppercase()
}

impl Desks {
    /// From crate::vdesk::sure() (None: not sure).
    pub fn of(s: Option<&Sure>, per_desktop: bool) -> Desks {
        let seen = match s {
            None => Seen::Unknown,
            Some(s) if s.ids.len() <= 1 => Seen::One(s.ids.first().map(guid_text)),
            Some(s) => match s.current_id() {
                Some(c) => Seen::Many { current: guid_text(&c), all: s.ids.iter().map(guid_text).collect() },
                None => Seen::Unknown,
            },
        };
        Desks { per_desktop, seen }
    }

    /// The current desktop (None: not known, or a single desktop Windows
    /// lists none for).
    pub fn current(&self) -> Option<&str> {
        match &self.seen {
            Seen::Many { current, .. } => Some(current),
            Seen::One(g) => g.as_deref(),
            Seen::Unknown => None,
        }
    }

    /// Does a note of `home` show now? Hidden only on a sure reading of
    /// two desktops or more, the setting on, and another desktop's note.
    pub fn shows(&self, home: Option<&str>) -> bool {
        match (home, &self.seen) {
            (Some(h), Seen::Many { current, .. }) if self.per_desktop => norm(h) == *current,
            _ => true,
        }
    }

    /// A new note's home: the desktop it is made on (whatever the setting,
    /// so turning it on later sorts the notes out); every desktop when
    /// that is not known.
    pub fn home_for_new(&self) -> Option<String> {
        self.current().map(String::from)
    }

    /// Is `home` a desktop that surely no longer exists (its note then
    /// belongs to every desktop)?
    pub fn gone(&self, home: Option<&str>) -> bool {
        let Some(h) = home else { return false };
        match &self.seen {
            Seen::Many { all, .. } => !all.contains(&norm(h)),
            Seen::One(Some(g)) => norm(h) != *g,
            Seen::One(None) => true,
            Seen::Unknown => false,
        }
    }

    /// Every home in `homes` of a desktop that no longer exists becomes
    /// every desktop. True: one did (the caller writes notes.json now).
    pub fn rehome<'a>(&self, homes: impl IntoIterator<Item = &'a mut Option<String>>) -> bool {
        let mut moved = false;
        for h in homes {
            if self.gone(h.as_deref()) {
                *h = None;
                moved = true;
            }
        }
        moved
    }

    /// The notes read at start (their homes, settled in place): a closed
    /// desktop's go to every desktop before their windows are made, so
    /// they show (review 3). (moved: notes.json is written at once, and
    /// whether each shows.)
    pub fn settle_loaded(&self, homes: &mut [Option<String>]) -> (bool, Vec<bool>) {
        let moved = self.rehome(homes.iter_mut());
        (moved, homes.iter().map(|h| self.shows(h.as_deref())).collect())
    }

    /// The note menu's one row ("Show on all desktops") means something:
    /// the setting on and two desktops or more.
    pub fn menu(&self) -> bool {
        self.per_desktop && matches!(self.seen, Seen::Many { .. })
    }

    /// The row picked on a note of `home`: every desktop becomes this
    /// desktop and back. None: nothing to do (no menu now).
    pub fn toggled(&self, home: Option<&str>) -> Option<Option<String>> {
        if !self.menu() {
            return None;
        }
        let cur = self.current()?;
        Some(match home {
            None => Some(cur.to_string()),
            Some(_) => None,
        })
    }
}

/// What a note's window needs now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WinOp {
    /// Shown (in front when summoned, else on the desktop layer).
    Show { front: bool },
    Hide,
    /// Visible but topmost while the notes are on the desktop: down.
    Lower,
    /// Visible but not in front while summoned: up.
    Raise,
    Keep,
}

/// Pure: a window whose note `shows`, now `visible` and `topmost`, the
/// notes `summoned` or not. Showing and the layer are set apart, so a
/// window that stays visible is still put on the right layer (review #5).
pub fn window_op(shows: bool, visible: bool, topmost: bool, summoned: bool) -> WinOp {
    match (shows, visible) {
        (true, false) => WinOp::Show { front: summoned },
        (false, true) => WinOp::Hide,
        (false, false) => WinOp::Keep,
        (true, true) if !summoned && topmost => WinOp::Lower,
        (true, true) if summoned && !topmost => WinOp::Raise,
        (true, true) => WinOp::Keep,
    }
}

/// Pure: the desktops changed while the notes are `summoned`: sent back
/// first (before the new reading decides what shows), when the current
/// desktop is another one now (left, or closed under them).
pub fn send_back_first(before: &Desks, now: &Desks, summoned: bool) -> bool {
    summoned && before.current() != now.current()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(b: u8) -> [u8; 16] {
        [b; 16]
    }

    fn sure(ids: &[u8], cur: usize) -> Sure {
        Sure { ids: ids.iter().map(|b| g(*b)).collect(), current: cur }
    }

    fn two(cur: usize) -> Desks {
        Desks::of(Some(&sure(&[1, 2], cur)), true)
    }

    #[test]
    fn each_desktop_shows_its_own_and_the_shared_notes() {
        let a = guid_text(&g(1));
        let b = guid_text(&g(2));
        let d = two(0);
        assert_eq!(d.current(), Some(a.as_str()));
        assert!(d.shows(None) && d.shows(Some(&a)) && !d.shows(Some(&b)));
        // Stored in another case: the same desktop.
        assert!(d.shows(Some(&a.to_lowercase())));
        let d = two(1);
        assert!(d.shows(None) && !d.shows(Some(&a)) && d.shows(Some(&b)));
        assert_eq!(d.home_for_new(), Some(b.clone()));
        // Off: everything everywhere (new notes still remember their desktop).
        let off = Desks::of(Some(&sure(&[1, 2], 1)), false);
        assert!(off.shows(Some(&a)) && off.shows(Some(&b)) && off.shows(None));
        assert_eq!(off.home_for_new(), Some(b));
        assert!(!off.menu());
        assert_eq!(off.toggled(None), None);
    }

    /// Review #1: nothing sure, nothing hidden, nothing moved.
    #[test]
    fn an_unsure_reading_shows_everything() {
        let a = guid_text(&g(1));
        let k = Desks::of(None, true);
        assert_eq!(k.seen, Seen::Unknown);
        assert!(k.shows(Some(&a)) && k.shows(Some("garbage")) && k.shows(None));
        assert_eq!(k.home_for_new(), None, "a new note belongs to every desktop");
        assert!(!k.gone(Some(&a)), "nothing is moved from a missing reading");
        assert!(!k.menu());
        assert_eq!(k.toggled(None), None);
        // Two listed, the current one not found: not known either.
        assert_eq!(Desks::of(Some(&sure(&[1, 2], 5)), true).seen, Seen::Unknown);
    }

    /// Review #4: a single desktop, surely read, is known: its notes stay
    /// its own, closed desktops' notes go to every desktop.
    #[test]
    fn a_single_desktop_is_known() {
        let a = guid_text(&g(1));
        let b = guid_text(&g(2));
        let one = Desks::of(Some(&sure(&[1], 0)), true);
        assert_eq!(one.seen, Seen::One(Some(a.clone())));
        assert!(one.shows(Some(&a)) && one.shows(Some(&b)), "one desktop: everything shows");
        assert_eq!(one.home_for_new(), Some(a.clone()), "made on it, kept on it when desktops are added");
        assert!(!one.gone(Some(&a)) && one.gone(Some(&b)));
        assert!(!one.menu());
        // Windows has never listed a desktop: any desktop a note names is gone.
        let never = Desks::of(Some(&sure(&[], 0)), true);
        assert_eq!(never.seen, Seen::One(None));
        assert_eq!(never.home_for_new(), None);
        assert!(never.gone(Some(&a)) && !never.gone(None));
    }

    #[test]
    fn a_closed_desktops_notes_go_to_every_desktop() {
        let d = two(0);
        assert!(!d.gone(Some(&guid_text(&g(2)))));
        assert!(d.gone(Some(&guid_text(&g(3)))));
        assert!(d.gone(Some("not a guid")));
        assert!(!d.gone(None));
        let mut homes = vec![Some(guid_text(&g(1))), Some(guid_text(&g(3))), None];
        assert!(d.rehome(homes.iter_mut()));
        assert_eq!(homes, vec![Some(guid_text(&g(1))), None, None]);
        assert!(!d.rehome(homes.iter_mut()), "nothing left to move");
        // Not known: nothing moves.
        let mut homes = [Some(guid_text(&g(3)))];
        assert!(!Desks::of(None, true).rehome(homes.iter_mut()));
        assert!(homes[0].is_some());
    }

    /// Review 3: a note of a desktop closed while NopBar was not running
    /// shows at start, and the move is written.
    #[test]
    fn notes_of_a_desktop_closed_meanwhile_show_at_start() {
        let d = two(0);
        let mut homes = vec![Some(guid_text(&g(3))), Some(guid_text(&g(2))), Some(guid_text(&g(1)))];
        let (moved, shows) = d.settle_loaded(&mut homes);
        assert!(moved);
        assert_eq!(homes[0], None);
        assert_eq!(shows, vec![true, false, true]);
        // Not sure at start: nothing moves, everything shows.
        let mut homes = [Some(guid_text(&g(3)))];
        assert_eq!(Desks::of(None, true).settle_loaded(&mut homes), (false, vec![true]));
    }

    #[test]
    fn the_row_toggles_between_this_desktop_and_all() {
        let d = two(1);
        let b = guid_text(&g(2));
        assert!(d.menu());
        assert_eq!(d.toggled(None), Some(Some(b.clone())));
        assert_eq!(d.toggled(Some(&b)), Some(None));
        // A note shown here belongs here or everywhere; either way it stays.
        for h in [None, Some(b)] {
            let next = d.toggled(h.as_deref()).unwrap();
            assert!(d.shows(next.as_deref()));
        }
    }

    #[test]
    fn showing_and_the_layer_are_set_apart() {
        assert_eq!(window_op(true, false, false, false), WinOp::Show { front: false });
        assert_eq!(window_op(true, false, true, true), WinOp::Show { front: true });
        assert_eq!(window_op(false, true, true, true), WinOp::Hide);
        assert_eq!(window_op(false, false, true, false), WinOp::Keep);
        assert_eq!(window_op(true, true, true, false), WinOp::Lower);
        assert_eq!(window_op(true, true, false, true), WinOp::Raise);
        assert_eq!(window_op(true, true, false, false), WinOp::Keep);
        assert_eq!(window_op(true, true, true, true), WinOp::Keep);
    }

    /// The hub's steps on a few windows: summoning, a click elsewhere
    /// (FocusLost) and the desktops changing, in either order. A window
    /// is (home, visible, topmost).
    struct Sim {
        desks: Desks,
        summoned: bool,
        wins: Vec<(Option<String>, bool, bool)>,
    }

    impl Sim {
        fn apply(&mut self) {
            for w in &mut self.wins {
                match window_op(self.desks.shows(w.0.as_deref()), w.1, w.2, self.summoned) {
                    WinOp::Show { front } => (w.1, w.2) = (true, front),
                    WinOp::Hide => w.1 = false,
                    WinOp::Lower => w.2 = false,
                    WinOp::Raise => w.2 = true,
                    WinOp::Keep => {}
                }
            }
        }
        fn summon(&mut self) {
            self.summoned = true;
            for w in &mut self.wins {
                if self.desks.shows(w.0.as_deref()) {
                    w.2 = true;
                }
            }
        }
        /// dismiss: every window down, shown or not.
        fn focus_lost(&mut self) {
            if self.summoned {
                self.summoned = false;
                for w in &mut self.wins {
                    w.2 = false;
                }
            }
        }
        /// desks_changed.
        fn desks(&mut self, now: Desks) {
            if send_back_first(&self.desks, &now, self.summoned) {
                self.focus_lost();
            }
            self.desks = now;
            let d = self.desks.clone();
            d.rehome(self.wins.iter_mut().map(|w| &mut w.0));
            self.apply();
        }
        fn check(&self, what: &str) {
            for (i, w) in self.wins.iter().enumerate() {
                assert_eq!(w.1, self.desks.shows(w.0.as_deref()), "{what}: window {i} visible");
                if !self.summoned && w.1 {
                    assert!(!w.2, "{what}: window {i} left in front");
                }
            }
        }
    }

    /// Review #5: summoned on A, A closed (B and C left): A's note goes to
    /// every desktop and ends on the desktop layer, whichever comes first.
    #[test]
    fn a_desktop_closed_under_summoned_notes_leaves_none_in_front() {
        let three = Desks::of(Some(&sure(&[1, 2, 3], 0)), true);
        let after = Desks::of(Some(&sure(&[2, 3], 0)), true);
        for desks_first in [true, false] {
            let mut s = Sim { desks: three.clone(), summoned: false, wins: vec![(Some(guid_text(&g(1))), false, false), (Some(guid_text(&g(2))), false, false), (None, false, false)] };
            s.apply();
            s.check("start");
            s.summon();
            if desks_first {
                s.desks(after.clone());
                // The desktop under them changed: sent back at once, every
                // window down, before anything else comes.
                assert!(!s.summoned, "sent back when the current desktop changes");
                assert!(s.wins.iter().all(|w| !w.2), "none left in front");
                s.focus_lost();
            } else {
                s.focus_lost();
                s.desks(after.clone());
            }
            s.check(if desks_first { "desktops first" } else { "focus first" });
            assert_eq!(s.wins[0].0, None, "A's note on every desktop");
            assert!(s.wins.iter().all(|w| w.1 && !w.2));
        }
        // Another desktop closed while summoned here: its notes come up in
        // front with the others, and go down together.
        let mut s = Sim { desks: Desks::of(Some(&sure(&[1, 2, 3], 1)), true), summoned: false, wins: vec![(Some(guid_text(&g(1))), false, false), (Some(guid_text(&g(2))), false, false)] };
        s.apply();
        s.summon();
        s.desks(Desks::of(Some(&sure(&[2, 3], 0)), true));
        assert!(s.summoned && s.wins.iter().all(|w| w.1 && w.2));
        s.focus_lost();
        s.check("sent back");
    }
}
