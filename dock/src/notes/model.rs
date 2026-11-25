//! Model test of saving, done at once on the notes' thread
//! (crate::notes::store: Doc::settle, delete, restore, settle_all, load,
//! and crate::notes::may_go for quitting), on a disk in memory with
//! faults: writes, replaces, moves and removals that fail at random or
//! all at once, a recovery folder that cannot be written, listings that
//! fail, the power going in the middle of work, notes.json lost or
//! broken, 31-day jumps. Quitting comes in every kind: the user's own
//! quit, another process's (Settings, an installer's --quit), an update
//! (put off once), a user's logoff and an installer's (Restart Manager).
//!
//! Also: a note's window destroyed by someone else (its editor can no
//! longer be read; the hub saves the text captured when it went, and may
//! or may not get a new window), notes.json failing on its own (the
//! layout stays to be written and counts when quitting), and the notes'
//! thread not answering a quit.
//!
//! The promises checked after every step:
//! - No text the user wrote is lost: each note's text once saved or kept
//!   — and, at every quit that went on, every text on screen but the ones
//!   that could be kept nowhere (only an installer, another process or a
//!   second update may go on with those) — is somewhere on disk (a note,
//!   a recovery copy, the trash) from then on. After such a quit and a
//!   start that reads everything, a live note's text is a note again.
//!   Only the trash past 30 days may go.
//! - Nothing automatic writes over or removes a file holding text (the
//!   disk itself checks: MemDisk::broken), but a note's own save, the
//!   index, trash past 30 days and a recovery copy whose text is a note.
//! - The user's own quit and a first update never go on while a text can
//!   be kept nowhere.
//!
//! Virtual desktops (crate::notes::desks) on top of all that: desktops
//! made, closed (down to one) and switched to, the registry unreadable or
//! the watch deaf for a while (crate::vdesk::sure gives nothing then), a
//! PC that has only ever had one desktop, the setting turned off and on,
//! a note moved between its desktop and every desktop (its menu),
//! summoning, and another program (a sync) changing places in notes.json.
//! The hub's rules are used as they are (Desks::shows, home_for_new,
//! rehome, toggled; store::merge for notes.json). Checked after every
//! step, besides the promises above, against the true desktops:
//! - Each desktop shows its own notes and those of every desktop, and
//!   only those; everything shows while the desktops are not surely known
//!   or the setting is off; summoning never shows another desktop's note.
//! - No note is left on a desktop that no longer exists (it goes to every
//!   desktop, and notes.json is written at once), whenever the reading is
//!   sure, a single desktop too; nothing moves on an unsure one.
//! - What another program wrote in notes.json and the hub did not change
//!   stays there.
//! - A note is only ever on the desktop it was made on (or the user moved
//!   it to), or on all of them.
//! - After a quit that wrote notes.json whole and a start that read it,
//!   every note is on the desktop it was on (notes.json lost or broken:
//!   on every desktop, never on another one).
//!
//! Texts carry their note's id, so a text found on disk is that note's.
//! A failure prints the seed, session and step, and the run's log.

use std::collections::HashMap;

use super::desks::Desks;
use super::store::{self, mem::MemDisk, Disk, Doc, Kept};
use super::{may_go, on_no_answer, Quit};
use crate::vdesk::{guid_text, Sure};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next() % n
        }
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
}

struct World {
    seed: u64,
    session: u32,
    step: u32,
    rng: Rng,
    disk: MemDisk,
    /// The notes on screen (`text`: what is saved, from text_now).
    docs: Vec<Doc>,
    /// What each note's editor shows (the user's text).
    view: HashMap<String, String>,
    /// Editors that can no longer be read (window destroyed), and the
    /// last text known of each note (the hub's `last`).
    dead: Vec<String>,
    last: HashMap<String, String>,
    /// notes.json holds the whole layout.
    layout_ok: bool,
    /// How often (%) the notes' thread does not answer a quit.
    no_answer: u64,
    /// Deleted notes for Ctrl+Z: (id, text, desktop).
    undo: Vec<(String, String, Option<String>)>,
    layout: store::Index,
    made: u32,
    edits: u32,
    now_secs: u64,
    fail: u64,
    /// An update was put off for the notes.
    put_off: bool,
    /// Texts that must stay on disk: key -> (text, deleted at).
    must: HashMap<String, (String, Option<u64>)>,
    log: Vec<String>,
    /// Windows' virtual desktops (their GUID bytes), the current one, and
    /// whether the registry can be read now.
    desktops: Vec<[u8; 16]>,
    cur: usize,
    registry: bool,
    /// The watch hears every change (else crate::vdesk::sure gives None).
    watch: bool,
    /// Windows has never listed a desktop (only ever one).
    never_listed: bool,
    made_desks: u8,
    /// Places another program wrote in notes.json (id -> x), and each
    /// note's place as the hub holds it (loaded, else the default).
    ext: HashMap<String, f32>,
    places: HashMap<String, crate::desklayer::place::Place>,
    /// The setting (Settings > widgets).
    per_desktop: bool,
    /// Each note's desktop (None: every desktop), as the hub holds it, and
    /// every desktop it was ever put on (made there, or moved there).
    home: HashMap<String, Option<String>>,
    been: HashMap<String, Vec<String>>,
    /// The last quit wrote notes.json whole: the next full start must give
    /// every note its desktop back.
    layout_whole: bool,
}

impl World {
    fn new(seed: u64) -> World {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let fail = rng.below(30);
        World {
            seed,
            session: 0,
            step: 0,
            rng,
            disk: MemDisk::default(),
            docs: Vec::new(),
            view: HashMap::new(),
            dead: Vec::new(),
            last: HashMap::new(),
            layout_ok: true,
            no_answer: 5,
            undo: Vec::new(),
            layout: store::Index::default(),
            made: 0,
            edits: 0,
            now_secs: 100_000,
            fail,
            put_off: false,
            must: HashMap::new(),
            log: Vec::new(),
            desktops: vec![[1; 16]],
            cur: 0,
            registry: true,
            watch: true,
            never_listed: true,
            made_desks: 1,
            ext: HashMap::new(),
            places: HashMap::new(),
            per_desktop: true,
            home: HashMap::new(),
            been: HashMap::new(),
            layout_whole: false,
        }
    }

    /// The reading is sure (read, and every change heard).
    fn sure(&self) -> bool {
        self.registry && self.watch
    }

    /// What crate::vdesk::sure() would give now.
    fn read_sure(&self) -> Option<Sure> {
        if !self.sure() {
            return None;
        }
        let ids = if self.never_listed { Vec::new() } else { self.desktops.clone() };
        Some(Sure { ids, current: self.cur })
    }

    /// The hub's view of the desktops.
    fn desks_now(&self) -> Desks {
        Desks::of(self.read_sure().as_ref(), self.per_desktop)
    }

    /// The true current desktop, as notes name it (None: Windows lists none).
    fn here(&self) -> Option<String> {
        (!self.never_listed).then(|| guid_text(&self.desktops[self.cur]))
    }

    fn home_of(&self, id: &str) -> Option<String> {
        self.home.get(id).cloned().flatten()
    }

    /// The notes shown on this desktop now.
    fn shown(&self) -> Vec<String> {
        let v = self.desks_now();
        self.docs.iter().filter(|d| v.shows(self.home_of(&d.id).as_deref())).map(|d| d.id.clone()).collect()
    }

    /// The hub's desks_changed / load: notes (and deleted ones kept for
    /// Ctrl+Z) of closed desktops go to every desktop, written at once.
    fn rehome(&mut self) {
        let v = self.desks_now();
        let ids: Vec<String> = self.docs.iter().map(|d| d.id.clone()).collect();
        let mut homes: Vec<Option<String>> = ids.iter().map(|id| self.home_of(id)).collect();
        let mut gone: Vec<Option<String>> = self.undo.iter().map(|u| u.2.clone()).collect();
        let before: Vec<Option<String>> = homes.iter().chain(gone.iter()).cloned().collect();
        let moved = v.rehome(homes.iter_mut().chain(gone.iter_mut()));
        // Only a desktop that is really closed, seen in a sure reading,
        // sends its notes to every desktop (never a registry that cannot be
        // read, nor a deaf watch).
        let truly: Vec<String> = self.desktops.iter().map(guid_text).collect();
        for (was, now) in before.iter().zip(homes.iter().chain(gone.iter())) {
            if was != now {
                let closed = was.as_ref().is_some_and(|h| !truly.contains(h));
                if !closed || !self.sure() {
                    self.fail_now(&format!("{was:?} sent to every desktop though it is {}", if closed { "not known to be closed" } else { "still there" }));
                }
            }
        }
        for (id, h) in ids.iter().zip(homes) {
            self.home.insert(id.clone(), h);
        }
        for (u, h) in self.undo.iter_mut().zip(gone) {
            u.2 = h;
        }
        if moved {
            // Written at once (calm: the save itself is what is checked).
            self.log.push("rehomed: a closed desktop's notes on every desktop".into());
            self.read_texts();
            self.faults();
            let now = self.now_ms();
            self.settle_everything(now);
            self.calm();
            if self.layout_ok {
                let ix = self.disk.read("", store::INDEX).map(|t| store::parse_index(&t)).unwrap_or_default();
                for (id, h) in &self.home {
                    if let Some(e) = ix.notes.iter().find(|e| e.id == *id) {
                        if e.desktop != *h {
                            self.fail_now(&format!("{id}'s move to every desktop not in notes.json ({:?})", e.desktop));
                        }
                    }
                }
            }
            self.check();
        }
    }

    /// The promises about desktops (see the module notes).
    fn check_desks(&self) {
        let shown = self.shown();
        let truly: Vec<String> = self.desktops.iter().map(guid_text).collect();
        for d in &self.docs {
            let h = self.home_of(&d.id);
            // Hidden only when sure, two desktops or more, the setting on,
            // and the note of another (true) desktop.
            let hide = self.sure() && self.desktops.len() >= 2 && !self.never_listed && self.per_desktop && h.is_some() && h != self.here();
            if shown.contains(&d.id) == hide {
                self.fail_now(&format!("{} (desktop {h:?}) shown {} on {:?} (setting {}, sure {})", d.id, !hide, self.here(), self.per_desktop, self.sure()));
            }
            if self.sure() && h.as_ref().is_some_and(|h| !truly.contains(h)) {
                self.fail_now(&format!("{} is left on a closed desktop {h:?}", d.id));
            }
            if !self.was_on(&d.id, h.as_deref()) {
                self.fail_now(&format!("{} is on {h:?}, never put there ({:?})", d.id, self.been.get(&d.id)));
            }
        }
    }

    /// Was note `id` ever put on `home` (every desktop: always)?
    fn was_on(&self, id: &str, home: Option<&str>) -> bool {
        home.is_none_or(|h| self.been.get(id).is_some_and(|b| b.iter().any(|x| x == h)))
    }

    /// Note `id` is put on `home` (made there, moved there).
    fn put(&mut self, id: &str, home: Option<String>) {
        if let Some(h) = &home {
            self.been.entry(id.to_string()).or_default().push(h.clone());
        }
        self.home.insert(id.to_string(), home);
    }

    /// A step about desktops.
    fn desk_step(&mut self) {
        match self.rng.below(7) {
            0 => {
                self.made_desks = self.made_desks.wrapping_add(1).max(1);
                self.desktops.push([self.made_desks; 16]);
                // From now on Windows lists them (one left too).
                self.never_listed = false;
                self.log.push(format!("desktop made ({})", self.desktops.len()));
            }
            1 if self.desktops.len() > 1 => {
                let i = self.rng.below(self.desktops.len() as u64) as usize;
                self.desktops.remove(i);
                if self.cur >= self.desktops.len() || self.cur == i {
                    self.cur = i.saturating_sub(1).min(self.desktops.len() - 1);
                } else if self.cur > i {
                    self.cur -= 1;
                }
                self.log.push(format!("desktop {i} closed ({} left)", self.desktops.len()));
            }
            2 | 3 => {
                self.cur = self.rng.below(self.desktops.len() as u64) as usize;
                self.log.push(format!("switched to desktop {}", self.cur));
            }
            4 if self.rng.chance(50) => {
                self.registry = !self.registry;
                self.log.push(format!("registry readable: {}", self.registry));
            }
            4 => {
                self.watch = !self.watch;
                self.log.push(format!("watch hears every change: {}", self.watch));
            }
            5 => {
                self.per_desktop = !self.per_desktop;
                self.log.push(format!("setting: {}", self.per_desktop));
            }
            _ => {
                // The menu of a note shown here.
                let v = self.desks_now();
                let shown = self.shown();
                if v.menu() && !shown.is_empty() {
                    let id = shown[self.rng.below(shown.len() as u64) as usize].clone();
                    if let Some(next) = v.toggled(self.home_of(&id).as_deref()) {
                        self.log.push(format!("menu {id}: {:?} -> {next:?}", self.home_of(&id)));
                        self.put(&id, next);
                        if !self.desks_now().shows(self.home_of(&id).as_deref()) {
                            self.fail_now(&format!("the menu hid {id}, shown here"));
                        }
                        self.save();
                    }
                }
            }
        }
        self.rehome();
        self.check_desks();
    }

    /// Another program (a sync) writes a note's place into notes.json.
    fn sync_edits(&mut self) {
        let Some(t) = self.disk.read("", store::INDEX) else { return };
        let Some(mut ix) = store::try_parse_index(&t) else { return };
        // A note whose text is there (an entry left behind by a delete is
        // dropped at the next write, whatever it says).
        let live: Vec<usize> = (0..ix.notes.len()).filter(|&k| self.disk.exists("", &store::body_name(&ix.notes[k].id)) == Some(true)).collect();
        if live.is_empty() {
            return;
        }
        let k = live[self.rng.below(live.len() as u64) as usize];
        let x = 1.0 + self.rng.below(500) as f32;
        ix.notes[k].place.x = x;
        let id = ix.notes[k].id.clone();
        self.disk.files.insert((String::new(), store::INDEX.into()), store::index_text(&ix));
        self.ext.insert(id.clone(), x);
        self.log.push(format!("sync: {id} at x {x}"));
        self.check();
    }

    /// The shortcut: this desktop's notes come up (a new one when there is
    /// none here), never another desktop's.
    fn summon(&mut self) {
        if self.shown().is_empty() {
            self.made += 1;
            let id = format!("n{}", self.made);
            let home = self.desks_now().home_for_new();
            self.log.push(format!("summon: new {id} on {home:?}"));
            self.view.insert(id.clone(), format!("{id}:"));
            self.docs.push(Doc { id: id.clone(), ..Default::default() });
            self.put(&id, home);
        }
        let v = self.desks_now();
        let up = self.shown();
        if up.is_empty() {
            self.fail_now("summoning shows nothing");
        }
        for id in &up {
            let h = self.home_of(id);
            if v.per_desktop && self.sure() && self.desktops.len() >= 2 && h.is_some() && h != self.here() {
                self.fail_now(&format!("summoning showed {id} of another desktop {h:?}"));
            }
        }
    }

    fn now_ms(&self) -> u128 {
        self.now_secs as u128 * 1000 + self.step as u128
    }

    fn fail_now(&self, what: &str) -> ! {
        let tail: Vec<&String> = self.log.iter().rev().take(60).collect::<Vec<_>>().into_iter().rev().collect();
        panic!(
            "seed {} session {} step {}: {what}\nfiles {:?}\n{}",
            self.seed,
            self.session,
            self.step,
            self.disk.files,
            tail.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n")
        );
    }

    fn check(&self) {
        if let Some(b) = self.disk.broken.first() {
            self.fail_now(&format!("an automatic step {b}"));
        }
        let all = self.disk.all_texts();
        for (key, (t, _)) in &self.must {
            if !all.contains(t) {
                self.fail_now(&format!("{key}'s text {t:?} is gone"));
            }
        }
        // What a sync wrote and the hub did not change stays (the hub never
        // moves a note in this model).
        if let Some(ix) = self.disk.read("", store::INDEX).and_then(|t| store::try_parse_index(&t)) {
            for (id, x) in &self.ext {
                if let Some(e) = ix.notes.iter().find(|e| e.id == *id) {
                    if e.place.x != *x {
                        self.fail_now(&format!("{id}'s place from a sync (x {x}) written over with {}", e.place.x));
                    }
                }
            }
        }
    }

    /// Faults for the next disk work.
    fn faults(&mut self) {
        self.disk.chaos = self.fail;
        self.disk.rng.set(self.rng.next());
        self.disk.now_secs = self.now_secs;
    }

    fn calm(&mut self) {
        self.disk.chaos = 0;
    }

    fn settled(&mut self, i: usize, k: Kept) {
        if k != Kept::Lost {
            let d = &self.docs[i];
            self.must.insert(d.id.clone(), (d.text.clone(), None));
        }
    }

    /// The hub's docs(): each note's text to save, from its editor or,
    /// when that cannot be read, the last text known.
    fn read_texts(&mut self) {
        for d in &mut self.docs {
            let read = (!self.dead.contains(&d.id)).then(|| self.view.get(&d.id).cloned().unwrap_or_default());
            let last = self.last.entry(d.id.clone()).or_default();
            d.text = store::text_now(read, last);
        }
    }

    /// What the hub's settle_now does: every note, then the layout, with
    /// the same store::settle_with_layout (the lost ids it gives are what
    /// quitting decides on).
    fn settle_everything(&mut self, now: u128) -> Vec<String> {
        let layout = store::Index { version: 1, seeded: false, notes: self.docs.iter().map(|d| store::Entry { size:None, hidden:false, id: d.id.clone(), desktop: self.home_of(&d.id), place: self.places.get(&d.id).copied().unwrap_or_default() }).collect(), last: String::new() };
        let old = self.layout.clone();
        let s = store::settle_with_layout(&mut self.disk, &mut self.docs, Some((&layout, &old)), now);
        // As the hub: the layout it asked for is what it last wrote.
        if s.index.is_some() {
            self.layout = layout;
        }
        self.layout_ok = s.layout_done;
        for i in 0..self.docs.len() {
            if !s.lost.contains(&self.docs[i].id) {
                self.settled(i, Kept::Saved);
            }
        }
        s.lost
    }

    /// The hub's save timer.
    fn save(&mut self) {
        self.read_texts();
        self.faults();
        let now = self.now_ms();
        self.settle_everything(now);
        self.calm();
        self.log.push(format!("save (layout {})", self.layout_ok));
        self.check();
    }

    fn pick(&mut self) -> Option<usize> {
        let n = self.docs.len() as u64;
        (n > 0).then(|| self.rng.below(n) as usize)
    }

    fn step(&mut self) {
        self.step += 1;
        match self.rng.below(17) {
            0 => {
                self.made += 1;
                let id = format!("n{}", self.made);
                let home = self.desks_now().home_for_new();
                // On the desktop it is made on (every desktop when that is
                // not known).
                let here = if self.sure() { self.here() } else { None };
                if home != here {
                    self.fail_now(&format!("a new note on {home:?}, made on {here:?}"));
                }
                self.log.push(format!("new {id} on {home:?}"));
                self.view.insert(id.clone(), format!("{id}:"));
                self.docs.push(Doc { id: id.clone(), ..Default::default() });
                self.put(&id, home);
            }
            1..=4 => {
                if let Some(i) = self.pick() {
                    let id = self.docs[i].id.clone();
                    if self.dead.contains(&id) {
                        // Its window was destroyed: made again (or not).
                        if self.rng.chance(50) {
                            self.dead.retain(|d| *d != id);
                            let t = self.last.get(&id).cloned().unwrap_or_default();
                            self.view.insert(id.clone(), t);
                            self.log.push(format!("window again {id}"));
                        }
                    } else {
                        self.edits += 1;
                        let e = self.edits;
                        let t = self.view.entry(id.clone()).or_default();
                        t.push_str(&format!(" {e}"));
                        self.log.push(format!("edit {id} = {t:?}"));
                    }
                }
            }
            5 => {
                if let Some(i) = self.pick() {
                    self.read_texts();
                    self.faults();
                    let now = self.now_ms();
                    let mut d = self.docs[i].clone();
                    let ok = store::delete(&mut self.disk, &mut d, now);
                    self.calm();
                    self.log.push(format!("delete {} -> {ok}", d.id));
                    if ok {
                        self.docs.remove(i);
                        self.view.remove(&d.id);
                        self.dead.retain(|x| *x != d.id);
                        self.must.insert(d.id.clone(), (d.text.clone(), Some(self.now_secs)));
                        let home = self.home.remove(&d.id).flatten();
                        self.ext.remove(&d.id);
                        self.undo.push((d.id, d.text, home));
                    } else {
                        // Nothing changed; what settle did stays known.
                        self.docs[i] = d;
                    }
                    self.check();
                }
            }
            6 => {
                // Ctrl+Z: the last note deleted that would show here.
                let v = self.desks_now();
                let k = self.undo.iter().rposition(|u| v.shows(u.2.as_deref()));
                if let Some((id, t, home)) = k.map(|k| self.undo.remove(k)) {
                    self.faults();
                    let now = self.now_ms();
                    let back = store::restore(&mut self.disk, &id, &t, now);
                    self.calm();
                    self.log.push(format!("restore {id} -> {}", back.is_some()));
                    match back {
                        Some(d) => {
                            if !v.shows(home.as_deref()) {
                                self.fail_now(&format!("Ctrl+Z brought back {id} of another desktop, unseen"));
                            }
                            self.home.insert(d.id.clone(), home);
                            self.view.insert(d.id.clone(), d.text.clone());
                            self.last.insert(d.id.clone(), d.text.clone());
                            self.docs.push(d);
                            let i = self.docs.len() - 1;
                            // Live once its file or a recovery copy has it.
                            if !self.docs[i].dirty() || self.docs[i].kept.as_deref() == Some(t.as_str()) {
                                self.settled(i, Kept::Saved);
                            }
                        }
                        None => self.undo.push((id, t, home)),
                    }
                    self.check();
                }
            }
            7..=9 => self.save(),
            10 => {
                // A folder that cannot be written for a while.
                self.disk.fail_sub = match self.rng.below(4) {
                    0 => Some(store::RECOVERY.into()),
                    1 => Some(String::new()),
                    _ => None,
                };
                self.disk.fail_writes = self.rng.chance(5);
            }
            11 => {
                // Someone destroys a note's window: its text is captured as
                // it goes; the Lost message comes later (maybe after a quit).
                if let Some(i) = self.pick() {
                    let id = self.docs[i].id.clone();
                    if !self.dead.contains(&id) {
                        let t = self.view.get(&id).cloned().unwrap_or_default();
                        self.last.insert(id.clone(), t);
                        self.dead.push(id.clone());
                        self.log.push(format!("window destroyed {id}"));
                    }
                }
                // notes.json alone may fail for a while.
                self.disk.fail_index = self.rng.chance(20);
            }
            13 | 14 => self.desk_step(),
            15 if self.rng.chance(70) => self.summon(),
            15 => self.sync_edits(),
            _ => {
                if self.rng.chance(3) {
                    self.crash();
                }
            }
        }
        self.check_desks();
    }

    /// Quitting of `kind` (`user_logoff`: a logoff the user started, not
    /// an installer's). True when it went on (then a start follows).
    fn quit(&mut self, kind: Quit) -> bool {
        let kind = match kind {
            Quit::Update if self.put_off => Quit::UpdateAgain,
            k => k,
        };
        // The notes' thread may not answer at all.
        if self.rng.chance(self.no_answer) {
            let go = on_no_answer(kind);
            self.log.push(format!("quit {kind:?}: no answer -> {go}"));
            if !go {
                return false;
            }
            // Gone without saving: only what was already on disk stays.
            self.layout_whole = false;
            let safe_ids: Vec<String> = self.docs.iter().filter(|d| !d.dirty()).map(|d| d.id.clone()).collect();
            self.start(Some(safe_ids));
            return true;
        }
        self.read_texts();
        self.faults();
        let now = self.now_ms() + 900;
        let lost = self.settle_everything(now);
        self.calm();
        let go = may_go(kind, lost.len());
        self.layout_whole = go && !lost.iter().any(|l| l == store::LAYOUT);
        // The user's own quit went on: the whole layout is on disk.
        if go && kind == Quit::User {
            let listed: Vec<String> = self.disk.read("", store::INDEX).map(|t| store::parse_index(&t).notes.into_iter().map(|e| e.id).collect()).unwrap_or_default();
            if let Some(d) = self.docs.iter().find(|d| !listed.contains(&d.id)) {
                self.fail_now(&format!("the user's quit went on without {} in notes.json", d.id));
            }
        }
        self.log.push(format!("quit {kind:?}: lost {lost:?} -> {go}"));
        if matches!(kind, Quit::Update | Quit::UpdateAgain) {
            self.put_off = !go;
        }
        if !lost.is_empty() && go && matches!(kind, Quit::User | Quit::Update) {
            self.fail_now("a quit that must wait went on with a text kept nowhere");
        }
        self.check();
        if !go {
            return false;
        }
        // Gone: what could be kept nowhere goes with the process.
        let safe_ids: Vec<String> = self.docs.iter().filter(|d| !lost.contains(&d.id)).map(|d| d.id.clone()).collect();
        self.start(Some(safe_ids));
        true
    }

    fn crash(&mut self) {
        self.log.push("--- crash ---".into());
        self.layout_whole = false;
        self.disk.power = Some(self.rng.below(4) as usize);
        self.save();
        self.disk.power = None;
        self.start(None);
    }

    /// A start; `safe`: after a quit that went on, the notes whose text
    /// was kept (they must be notes again).
    fn start(&mut self, safe: Option<Vec<String>>) {
        self.disk.fail_sub = None;
        self.disk.fail_writes = false;
        self.disk.fail_index = false;
        if self.rng.chance(15) {
            self.now_secs += 31 * 86_400;
            self.log.push("--- 31 days later ---".into());
            let now = self.now_secs;
            self.must.retain(|_, (_, d)| d.is_none_or(|at| now.saturating_sub(at) < store::TRASH_DAYS * 86_400));
        }
        let mut damaged = false;
        if self.rng.chance(6) {
            damaged = true;
            self.ext.clear();
            if self.rng.chance(50) {
                self.disk.files.remove(&(String::new(), store::INDEX.to_string()));
                self.log.push("--- notes.json lost ---".into());
            } else {
                self.disk.files.insert((String::new(), store::INDEX.to_string()), "{\"notes\": [".into());
                self.log.push("--- notes.json broken ---".into());
            }
        }
        self.now_secs += 60;
        let now = self.now_secs as u128 * 1000;
        // A start with faults (listings too) ...
        self.faults();
        let _ = store::load(&mut self.disk, now);
        self.calm();
        self.check();
        // ... and one that reads everything.
        let loaded = store::load(&mut self.disk, now + 500);
        self.check();
        let shown: Vec<&String> = loaded.notes.iter().map(|(_, t)| t).collect();
        if let Some(ids) = &safe {
            for d in self.docs.iter().filter(|d| ids.contains(&d.id)) {
                if !shown.contains(&&d.text) {
                    self.fail_now(&format!("{}'s text {:?} is not a note after a full start", d.id, d.text));
                }
            }
        }
        // Each note's desktop: the one it had when notes.json was written
        // whole and read back; otherwise that or every desktop, never
        // another one.
        for (e, _) in &loaded.notes {
            let had = self.home.get(&e.id).cloned().flatten();
            if self.layout_whole && !damaged && self.home.contains_key(&e.id) && e.desktop != had {
                self.fail_now(&format!("{} came back on {:?}, was on {had:?}", e.id, e.desktop));
            }
            if !self.was_on(&e.id, e.desktop.as_deref()) {
                self.fail_now(&format!("{} came back on {:?}, never put there ({:?})", e.id, e.desktop, self.been.get(&e.id)));
            }
        }
        self.layout_whole = false;
        self.home = loaded.notes.iter().map(|(e, _)| (e.id.clone(), e.desktop.clone())).collect();
        self.places = loaded.notes.iter().map(|(e, _)| (e.id.clone(), e.place)).collect();
        self.docs = loaded.notes.iter().map(|(e, t)| Doc { id: e.id.clone(), text: t.clone(), saved: Some(t.clone()), kept: None }).collect();
        self.view = self.docs.iter().map(|d| (d.id.clone(), d.text.clone())).collect();
        self.last = self.view.clone();
        self.dead.clear();
        self.layout_ok = true;
        self.layout = loaded.index.clone();
        // Deleted notes' texts stay required, unless they came back as
        // notes (imported from recovery): then their new note holds them.
        let live: Vec<String> = self.docs.iter().map(|d| d.text.clone()).collect();
        self.must.retain(|_, (t, d)| d.is_some() && !live.contains(t));
        for d in &self.docs {
            self.must.insert(d.id.clone(), (d.text.clone(), None));
        }
        self.undo.clear();
        self.session += 1;
        self.step = 0;
        self.log.push(format!("--- start (session {}) ---", self.session));
        // The hub's load: closed desktops' notes to every desktop.
        self.rehome();
        self.check_desks();
    }

    fn session(&mut self) {
        let steps = 20 + self.rng.below(100);
        for _ in 0..steps {
            self.step();
        }
        let kind = match self.rng.below(6) {
            0 => Quit::External,
            1 => Quit::Update,
            // A logoff: the user's (held up like the user's quit) or an
            // installer's (never held up).
            2 => Quit::External,
            _ => Quit::User,
        };
        for attempt in 0.. {
            if attempt > 40 {
                self.fail_now("quitting never went on");
            }
            if self.quit(kind) {
                return;
            }
            // Called off: the notes are shown; the user goes on, the disk
            // may come back.
            self.disk.fail_sub = None;
            self.disk.fail_writes = false;
            self.disk.fail_index = false;
            for _ in 0..self.rng.below(6) {
                self.step();
            }
        }
    }
}

#[test]
fn no_text_is_ever_lost_and_nothing_is_written_over() {
    for seed in 1..=500u64 {
        let mut w = World::new(seed);
        for _ in 0..4 {
            w.session();
        }
    }
}

/// S4: the user's quit is called off (before anything goes) while a text
/// can be kept nowhere, and goes on once it can; an update is put off
/// once, then goes on.
#[test]
fn a_quit_waits_only_while_a_text_can_be_kept_nowhere() {
    let mut w = World::new(5);
    w.fail = 0;
    w.no_answer = 0;
    w.view.insert("a".into(), "a: typed".into());
    w.docs.push(Doc { id: "a".into(), ..Default::default() });
    w.disk.fail_writes = true;
    assert!(!w.quit(Quit::User), "called off");
    assert!(!w.quit(Quit::Update), "put off once");
    w.disk.fail_writes = true;
    assert!(w.quit(Quit::Update), "the next try goes on");
    let mut w = World::new(6);
    w.fail = 0;
    w.no_answer = 0;
    w.view.insert("b".into(), "b: typed".into());
    w.docs.push(Doc { id: "b".into(), ..Default::default() });
    w.disk.fail_writes = true;
    assert!(!w.quit(Quit::User));
    w.disk.fail_writes = false;
    assert!(w.quit(Quit::User));
}

/// S3: another process's quit (an installer) is never held up; what can
/// be kept is kept first (the second recovery folder too).
#[test]
fn an_installer_is_never_held_up() {
    let mut w = World::new(7);
    w.fail = 0;
    w.no_answer = 0;
    w.docs.push(Doc { id: "c".into(), text: "c: typed".into(), ..Default::default() });
    w.view.insert("c".into(), "c: typed".into());
    w.disk.fail_replaces = true;
    w.disk.fail_sub = Some(store::RECOVERY.into());
    let now = w.now_ms();
    let lost = store::settle_all(&mut w.disk, &mut w.docs, now);
    assert!(lost.is_empty(), "kept in the local folder");
    assert_eq!(w.disk.list(store::RESCUE).unwrap().len(), 1);
    assert!(may_go(Quit::External, 1));
}

/// T1 by hand: a window destroyed just before a quit (its Lost message
/// not handled yet): the text captured as it went is saved, never an
/// empty one.
#[test]
fn a_destroyed_window_before_a_quit_keeps_its_text() {
    let mut w = World::new(8);
    w.fail = 0;
    w.no_answer = 0;
    w.view.insert("d".into(), "d: saved".into());
    w.docs.push(Doc { id: "d".into(), ..Default::default() });
    w.save();
    w.view.insert("d".into(), "d: saved more".into());
    w.last.insert("d".into(), "d: saved more".into());
    w.dead.push("d".into());
    assert!(w.quit(Quit::User));
    assert!(w.disk.all_texts().contains(&"d: saved more".to_string()));
}

/// T2 by hand: notes.json failing alone keeps the user's quit waiting
/// (the layout counts), and lets it go once written.
#[test]
fn a_layout_that_cannot_be_written_holds_the_users_quit() {
    let mut w = World::new(9);
    w.fail = 0;
    w.no_answer = 0;
    w.view.insert("e".into(), "e: x".into());
    w.docs.push(Doc { id: "e".into(), ..Default::default() });
    w.disk.fail_index = true;
    assert!(!w.quit(Quit::User));
    w.disk.fail_index = false;
    assert!(w.quit(Quit::User));
}

/// 7th review by hand: a new note whose file cannot be written (its text
/// in recovery) leaves notes.json written without it: the user's quit
/// waits for the layout too.
#[test]
fn a_layout_written_without_a_note_holds_the_users_quit() {
    let mut w = World::new(10);
    w.fail = 0;
    w.no_answer = 0;
    w.view.insert("f".into(), "f: x".into());
    w.docs.push(Doc { id: "f".into(), ..Default::default() });
    w.disk.fail_replaces = true;
    w.disk.fail_index = false;
    assert!(!w.quit(Quit::User), "notes.json lacks f");
    w.disk.fail_replaces = false;
    assert!(w.quit(Quit::User));
}
