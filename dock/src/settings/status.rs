//! A line under a settings row when the setting is saved but does not
//! work as chosen (live status only; what a setting also does is said
//! behind the row's "i": App::row_aside): a shortcut another app holds or that clashes with
//! another of NopBar's, "start with Windows" that Windows is not doing, an
//! effect that high contrast or Windows' animation setting turns off. Each
//! says why and, where there is one,
//! what fixes it (a link under the text).

use super::App;
use crate::hotkeys::Status;
use crate::i18n::{t, tf};
use crate::shortcuts::{self, Chord};

/// What the link under the note does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fix {
    /// Windows' Settings > Apps > Startup.
    StartupApps,
}

impl Fix {
    pub(super) fn label(self) -> &'static str {
        match self {
            Fix::StartupApps => t("settings.startup.open"),
        }
    }

    pub(super) fn run(self) {
        match self {
            Fix::StartupApps => {
                crate::launch::open(&crate::launch::Target::new("ms-settings:startupapps", "", crate::launch::Source::Settings));
            }
        }
    }
}

/// Pure: the note for "start with Windows". `wanted`: what was saved
/// (asked of Windows), `actual`: what Windows does now.
pub(super) fn startup_note(wanted: bool, actual: bool) -> Option<&'static str> {
    match (wanted, actual) {
        (true, false) => Some("settings.startup.not_on"),
        (false, true) => Some("settings.startup.not_off"),
        _ => None,
    }
}

/// Pure: "{key} is in use … Try {alts}." for `chord`, offering `alts`.
pub(super) fn in_use_note(chord: Chord, alts: &[Chord]) -> String {
    let shown = chord.display();
    if alts.is_empty() {
        tf("shortcuts.note.in_use_no_alt", &[("key", &shown)])
    } else {
        let alts: Vec<String> = alts.iter().map(|a| a.display()).collect();
        tf("shortcuts.note.in_use", &[("key", &shown), ("alts", &alts.join(", "))])
    }
}

/// Pure: the note for a shortcut row holding `spec`, given what the dock
/// reported (`reports`: row key, spec, status). Reports for another spec
/// (an older choice) say nothing about this one. `alts`: keys to offer
/// when it is taken.
pub(super) fn hotkey_note(key: &str, spec: &str, reports: &[(String, String, Status)], alts: &dyn Fn(Chord) -> Vec<Chord>) -> Option<String> {
    let p = |s: &str| shortcuts::parse(s).ok().flatten();
    let (_, _, st) = reports.iter().find(|(k, s, _)| k == key && (s.eq_ignore_ascii_case(spec) || (p(s).is_some() && p(s) == p(spec))))?;
    let shown = crate::hotkeys::display(spec);
    match st {
        Status::Taken => match p(spec) {
            Some(c) => Some(in_use_note(c, &alts(c))),
            // A digit set: some of its keys are another app's.
            None => Some(tf("settings.hotkey.taken_no_alt", &[("key", &shown)])),
        },
        Status::Invalid => Some(tf("settings.hotkey.invalid", &[("key", spec)])),
        Status::Failed => Some(tf("settings.hotkey.failed", &[("key", &shown)])),
        Status::Ok | Status::Off => None,
    }
}

impl App {
    /// The effects' system state as crate::effects sees it.
    pub(super) fn fx_env(&self) -> crate::effects::Env {
        crate::effects::Env { animations: self.motion, high_contrast: self.hc.is_some() }
    }

    /// Every chord NopBar uses except shortcut `id`'s.
    pub(super) fn chords_but(&self, id: &str) -> Vec<Chord> {
        shortcuts::ALL.iter().filter(|s| s.id != id).flat_map(|s| shortcuts::chords(s, &self.cfg.shortcut(s.id))).collect()
    }

    /// The note under row `key` (None: it works as chosen). A previewed
    /// hotkey is really registered, so its note shows at once; "start
    /// with Windows" changes only on Save, so an unsaved switch has none.
    pub(super) fn row_note(&self, key: &str) -> Option<(String, Option<Fix>)> {
        if key == "chat.login" {
            return Some((
                if cfg!(test) {
                    "Test account - fixture-client".into()
                } else {
                    crate::companion::chat::settings::note()
                },
                None,
            ));
        }
        if key == "chat.model" {
            return (!self.cfg.chat.model.is_empty()).then(|| (self.cfg.chat.model.clone(), None));
        }
        if key == "chat.daily_limit" {
            let used = if cfg!(test) {
                0
            } else {
                crate::companion::chat::transport::usage()
                    .today(crate::companion::chat::transport::local_day())
            };
            return Some((
                crate::i18n::t("chat.today").replace("{n}", &used.to_string()),
                None,
            ));
        }

        if key == "start_with_windows" {
            if self.touched.iter().any(|k| k == key) {
                return None;
            }
            let (wanted, actual) = self.startup?;
            return startup_note(wanted, actual).map(|k| (t(k).to_string(), Some(Fix::StartupApps)));
        }
        if let Some(id) = shortcuts::id_of_key(key) {
            return self.shortcut_note(id).map(|s| (s, None));
        }
        // The first-run flow's corner switch: on, but no corner does
        // anything yet.
        if self.welcome.is_some() && key == "hot_corners.enabled" && self.cfg.hot_corners.enabled {
            if let Some(k) = super::model::feature_note(&self.cfg, key) {
                return Some((t(k).to_string(), None));
            }
        }
        // A switch that is on but cannot show now (the island is off).
        if key == "topbar.island_screenshot" && self.cfg.topbar.island_screenshot {
            if let Some(k) = super::model::feature_note(&self.cfg, key) {
                return Some((t(k).to_string(), None));
            }
        }
        crate::effects::forced_off(key, &self.cfg, self.fx_env()).map(|k| (t(k).to_string(), None))
    }

    fn shortcut_note(&self, id: &str) -> Option<String> {
        // What the recorder just refused, until the next key or click.
        if let Some((rid, note)) = &self.rec_note {
            if *rid == id {
                return Some(note.clone());
            }
        }
        let v = self.cfg.shortcut(id);
        // Two of NopBar's own on the same keys.
        if let Some(other) = shortcuts::chords(shortcuts::find(id)?, &v).into_iter().find_map(|c| shortcuts::used_by(&self.cfg, id, c)) {
            return Some(tf("shortcuts.note.duplicate", &[("name", t(other.label))]));
        }
        let key = shortcuts::row_key(id);
        hotkey_note(key, &v, &self.client.hotkeys, &|c| self.suggestions(id, c))
    }

    /// What row `key` also does, set the way it is now, that is worth
    /// knowing but is not a problem: said behind its "i" (row_hint), never
    /// as a line under the row (the notes under rows are live status only:
    /// a clash, a refusal, something turned off). Win+digit: Windows' own
    /// keys, caught by the hook while on.
    pub(super) fn row_aside(&self, key: &str) -> Option<&'static str> {
        let id = shortcuts::id_of_key(key)?;
        let s = shortcuts::find(id)?;
        (s.kind == shortcuts::Kind::Digits && shortcuts::uses_hook(s, &self.cfg.shortcut(id))).then(|| t("shortcuts.note.hook"))
    }

    /// Keys to offer instead of `chord` for shortcut `id` (same key, other
    /// modifiers, free right now). Remembered per chord: the test
    /// registrations run once, not on every redraw.
    pub(super) fn suggestions(&self, id: &str, chord: Chord) -> Vec<Chord> {
        if let Some(v) = self.alts.borrow().iter().find(|(c, _)| *c == chord).map(|(_, v)| v.clone()) {
            return v;
        }
        let v = shortcuts::suggest(chord, &self.chords_but(id), &|c| self.chord_in_use(c));
        self.alts.borrow_mut().push((chord, v.clone()));
        v
    }

    /// Reads what Windows does about starting NopBar at sign-in; the
    /// switch shows that (`wanted`: what was saved).
    pub(super) fn verify_startup(&mut self, wanted: bool) {
        match crate::release::run_at_login_state() {
            Some(actual) => {
                self.startup = Some((wanted, actual));
                self.cfg.start_with_windows = actual;
            }
            None => self.startup = None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_notes() {
        assert_eq!(startup_note(true, true), None);
        assert_eq!(startup_note(false, false), None);
        assert_eq!(startup_note(true, false), Some("settings.startup.not_on"));
        assert_eq!(startup_note(false, true), Some("settings.startup.not_off"));
    }

    #[test]
    fn hotkey_notes() {
        let _l = crate::i18n::test_lock("en");
        let rep = |spec: &str, st| vec![(crate::hotkeys::SEARCH.to_string(), spec.to_string(), st)];
        let k = crate::hotkeys::SEARCH;
        let none = |_: Chord| Vec::new();
        let alt = |c: Chord| vec![Chord::new(shortcuts::CTRL | shortcuts::ALT, c.vk)];
        assert!(hotkey_note(k, "ctrl+space", &rep("ctrl+space", Status::Ok), &none).is_none());
        assert!(hotkey_note(k, "none", &rep("none", Status::Off), &none).is_none());
        let taken = hotkey_note(k, "ctrl+space", &rep("Ctrl + Space", Status::Taken), &alt).unwrap();
        assert!(taken.contains("Ctrl+Space") && taken.contains("Ctrl+Alt+Space"), "{taken}");
        let taken = hotkey_note(k, "ctrl+space", &rep("ctrl+space", Status::Taken), &none).unwrap();
        assert!(taken.contains("Ctrl+Space"), "{taken}");
        // A report about another (older) choice says nothing.
        assert!(hotkey_note(k, "alt+space", &rep("ctrl+space", Status::Taken), &none).is_none());
        assert!(hotkey_note(k, "alt+wat", &rep("alt+wat", Status::Invalid), &none).is_some());
        assert!(hotkey_note(crate::hotkeys::LAUNCHPAD, "alt+f1", &rep("alt+f1", Status::Taken), &none).is_none(), "another row's report");
        // A digit set some of whose keys are taken.
        let d = vec![(crate::hotkeys::DOCK_APPS.to_string(), "alt".to_string(), Status::Taken)];
        assert!(hotkey_note(crate::hotkeys::DOCK_APPS, "alt", &d, &none).is_some());
    }

    #[test]
    fn strings_exist() {
        for k in [
            "settings.startup.not_on",
            "settings.startup.not_off",
            "settings.startup.open",
            "settings.hotkey.taken_no_alt",
            "settings.hotkey.invalid",
            "settings.hotkey.failed",
            "shortcuts.note.in_use",
            "shortcuts.note.in_use_no_alt",
            "shortcuts.note.duplicate",
            "shortcuts.note.needs_modifier",
            "shortcuts.note.hook",
        ] {
            assert_ne!(t(k), k, "{k}");
        }
    }
}
