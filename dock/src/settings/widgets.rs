//! The widgets card (General page): the heading "Widgets" with, while a
//! widget is on, what they cost this PC as a word at its end ("Low load"),
//! and a row per widget kind with what it is and its shortcut under its
//! label. The numbers (memory,
//! CPU, the PC, each widget's load) are behind the heading's "i".
//!
//! What the dock measured comes over the pipe (crate::desklayer::load's
//! Status, with GetState and Preview); offline the heading shows no word.

use std::borrow::Cow;

use super::App;
use crate::config::Config;
use crate::desklayer::load::{Kind, Level, Status};
use crate::i18n::{t, tf};
use crate::shortcuts;

/// Pure: the widget kind a row is for ("widgets.notes" -> Notes).
pub fn kind_of(key: &str) -> Option<Kind> {
    Kind::from_id(key.strip_prefix("widgets.")?)
}

/// The shortcut that calls widget `k` up (the notes) or makes one (a
/// piece of the screen).
fn shortcut_of(k: Kind) -> &'static str {
    match k {
        Kind::Notes => shortcuts::NOTES,
        Kind::Pieces => shortcuts::SNIP,
    }
}

/// Pure: "Low load" / "Medium load" / "High load".
pub fn level_text(l: Level) -> &'static str {
    match l {
        Level::Light => t("widgets.load.light"),
        Level::Normal => t("widgets.load.normal"),
        Level::Heavy => t("widgets.load.heavy"),
    }
}

/// Pure: "12 MB" (whole MB; under one: "<1 MB").
pub fn mb(bytes: u64) -> String {
    let m = bytes as f64 / (1u64 << 20) as f64;
    if bytes > 0 && m < 1.0 {
        "<1 MB".into()
    } else {
        format!("{} MB", m.round() as u64)
    }
}

/// Pure: "0.12%" (under 0.01: "<0.01%").
pub fn pct(p: f32) -> String {
    if p > 0.0 && p < 0.01 {
        "<0.01%".into()
    } else {
        format!("{:.2}%", p.max(0.0))
    }
}

/// Pure: the kinds switched on in `c`, in the rows' order.
pub fn kinds_on(c: &Config) -> Vec<Kind> {
    Kind::ALL.into_iter().filter(|k| matches!(k, Kind::Notes if c.widgets.notes) || matches!(k, Kind::Pieces if c.widgets.pieces)).collect()
}

/// Pure: the word at the heading's end: none while every widget is off
/// (or offline); else their load.
pub fn header_status(any_on: bool, s: Option<&Status>) -> Option<&'static str> {
    s.filter(|_| any_on).map(|s| level_text(s.level))
}

/// Pure: the heading's "i": what the load is, then their load (the
/// heading's word, for a screen reader too) and what they use, what the
/// PC has, and each widget switched on (`on`) with its load.
pub fn summary_tip(s: Option<&Status>, on: &[Kind]) -> String {
    let mut lines = vec![t("settings.group.widgets.hint").to_string()];
    if let Some(s) = s {
        let usage = tf("widgets.usage", &[("mem", &mb(s.mem)), ("cpu", &pct(s.cpu))]);
        lines.push(if on.is_empty() { usage } else { format!("{} \u{00B7} {usage}", level_text(s.level)) });
        lines.push(tf("widgets.pc", &[("ram", &format!("{} GB", (s.spec.ram as f64 / (1u64 << 30) as f64).round() as u64)), ("cores", &s.spec.cores.to_string())]));
        for r in s.rows.iter().filter(|r| on.contains(&r.kind)) {
            lines.push(format!("{} \u{00B7} {}", t(&format!("widgets.{}", r.kind.id())), level_text(r.level)));
        }
    }
    lines.join("\n")
}

/// Pure: a widget row's grey line: what it is, then its shortcut as set
/// now (`chord`: none when switched off).
pub fn row_line(k: Kind, chord: Option<&str>) -> String {
    let what = t(&format!("widgets.{}.hint", k.id()));
    match chord {
        Some(c) => format!("{what} {}", tf("widgets.shortcut", &[("key", c)])),
        None => what.to_string(),
    }
}

impl App {
    /// The word at the widgets' heading's end (None: none drawn).
    pub(super) fn widgets_status(&self) -> Option<&'static str> {
        header_status(!kinds_on(&self.cfg).is_empty(), self.client.widgets.as_ref())
    }

    /// The widgets card's dynamic descriptions (None: not one of its rows).
    pub(super) fn widgets_hint(&self, key: &str) -> Option<Cow<'static, str>> {
        if key == super::model::WIDGETS_SUMMARY {
            return Some(Cow::Owned(summary_tip(self.client.widgets.as_ref(), &kinds_on(&self.cfg))));
        }
        let k = kind_of(key)?;
        let keys = self.widget_keys(k);
        let chord = (!keys.is_empty()).then(|| keys.join("+"));
        Some(Cow::Owned(row_line(k, chord.as_deref())))
    }

    /// The keys that call widget `k` up now, plainly ("Win", "Alt", "N";
    /// none when its shortcut is off): its line's text and its how-to
    /// card's keycaps.
    pub(super) fn widget_keys(&self, k: Kind) -> Vec<String> {
        shortcuts::plain_keys(&self.cfg.shortcut(shortcut_of(k)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desklayer::load::{Row, Spec, Usage};

    #[test]
    fn device_battery_defaults_and_conditional_row() {
        let mut c = Config::default();
        assert!(!c.widgets.device_battery && c.widgets.device_battery_alerts);
        assert!(super::super::model::sub_row_off(&c, "widgets.device_battery_alerts"));
        c.widgets.device_battery = true;
        assert!(!super::super::model::sub_row_off(&c, "widgets.device_battery_alerts"));
        let old: crate::config::WidgetsCfg = serde_json::from_str("{}").unwrap();
        assert!(!old.device_battery && old.device_battery_alerts && old.inbox);
        c.widgets.notes=false;
        assert!(super::super::model::sub_row_off(&c,"widgets.inbox"));
        c.widgets.inbox=false;
        let round: crate::config::WidgetsCfg = serde_json::from_str(&serde_json::to_string(&c.widgets).unwrap()).unwrap();
        assert_eq!(round, c.widgets);
    }

    fn status() -> Status {
        Status {
            count: 4,
            level: Level::Normal,
            mem: 30 << 20,
            cpu: 0.004,
            spec: Spec { ram: 16 << 30, cores: 8 },
            rows: vec![
                Row { kind: Kind::Notes, usage: Usage { count: 1, mem: 2 << 20, cpu: 0.003 }, level: Level::Light },
                Row { kind: Kind::Pieces, usage: Usage { count: 3, mem: 28 << 20, cpu: 0.001 }, level: Level::Normal },
            ],
        }
    }

    #[test]
    fn the_heading_says_the_load_only_with_a_widget_on() {
        let _l = crate::i18n::test_lock("en");
        let s = status();
        assert_eq!(header_status(true, Some(&s)), Some(level_text(Level::Normal)));
        assert_eq!(header_status(false, Some(&s)), None, "every widget off: no word");
        assert_eq!(header_status(true, None), None, "offline: no word");
        let mut c = Config::default();
        c.widgets.notes = false;
        c.widgets.pieces = true;
        assert_eq!(kinds_on(&c), [Kind::Pieces]);
        c.widgets.notes = true;
        assert_eq!(kinds_on(&c), [Kind::Notes, Kind::Pieces]);
        c.widgets.notes = false;
        c.widgets.pieces = false;
        assert!(kinds_on(&c).is_empty());
    }

    #[test]
    fn the_heading_s_i_has_the_numbers() {
        let _l = crate::i18n::test_lock("en");
        let s = status();
        let tip = summary_tip(Some(&s), &[Kind::Notes, Kind::Pieces]);
        assert!(tip.starts_with(t("settings.group.widgets.hint")), "what the load is first: {tip}");
        assert!(tip.contains("30 MB") && tip.contains("16 GB") && tip.contains('8'), "{tip}");
        let notes = format!("{} \u{00B7} {}", t("widgets.notes"), level_text(Level::Light));
        let pieces = format!("{} \u{00B7} {}", t("widgets.pieces"), level_text(Level::Normal));
        assert!(tip.contains(&notes) && tip.contains(&pieces), "each widget's load: {tip}");
        assert!(tip.contains(&format!("{} \u{00B7} ", level_text(Level::Normal))), "the heading's word: {tip}");
        assert!(!summary_tip(Some(&s), &[Kind::Notes]).contains(&pieces), "a widget off: not listed");
        assert_eq!(summary_tip(None, &[Kind::Notes]), t("settings.group.widgets.hint"));
    }

    #[test]
    fn a_widget_row_says_what_it_is_and_its_shortcut() {
        let _l = crate::i18n::test_lock("en");
        let line = row_line(Kind::Notes, Some("Win+Alt+N"));
        assert!(line.starts_with(t("widgets.notes.hint")) && line.contains("Win+Alt+N"), "{line}");
        assert_eq!(row_line(Kind::Pieces, None), t("widgets.pieces.hint"), "no shortcut: just what it is");
        assert_eq!(kind_of("widgets.notes"), Some(Kind::Notes));
        assert_eq!(kind_of("widgets.hide_from_capture"), None);
        assert_eq!(kind_of("widgets.notes_per_desktop"), None, "a setting of the notes, not a widget");
        assert_eq!(kind_of("notes"), None);
        for k in Kind::ALL {
            assert!(crate::shortcuts::find(shortcut_of(k)).is_some(), "{k:?}");
        }
    }

    #[test]
    fn numbers_read_short() {
        assert_eq!(mb(0), "0 MB");
        assert_eq!(mb(1000), "<1 MB");
        assert_eq!(mb(12 << 20), "12 MB");
        assert_eq!(pct(0.0), "0.00%");
        assert_eq!(pct(0.004), "<0.01%");
        assert_eq!(pct(1.234), "1.23%");
    }

    #[test]
    fn every_text_is_there() {
        for key in [
            "settings.group.widgets",
            "settings.group.widgets.hint",
            "widgets.usage",
            "widgets.pc",
            "widgets.load.light",
            "widgets.load.normal",
            "widgets.load.heavy",
            "widgets.shortcut",
            "widgets.notes",
            "widgets.notes.hint",
            "widgets.pieces",
            "widgets.pieces.hint",
            "widgets.hide_from_capture",
            "widgets.hide_from_capture.hint",
        ] {
            assert!(crate::i18n::get(key).is_some(), "{key}");
        }
    }
}
